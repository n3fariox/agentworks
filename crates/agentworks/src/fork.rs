//! L-08: forking carries recorded conversation into an independent new session.
//!
//! The child is independent: use an anchored native Codex fork when available,
//! otherwise explicitly seed a fresh provider session with transcript or summary.
//! No equivalence of hidden provider state is claimed. The original session is only read, never interrupted, and pending
//! approvals, queued inputs and Agentworks access are never copied.
use crate::{Session, fail};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Framing plus transcript ceiling for the context handed to a harness.
const PRIMER_LIMIT: usize = 192 * 1024;
/// Ceiling for one carried message inside the primer.
const MESSAGE_LIMIT: usize = 16 * 1024;
/// Most recent parent events inspected for a boundary, matching the transcript cap.
const BOUNDARY_SCAN: usize = crate::MAX_EVENTS;

type Error = (StatusCode, String);

pub use agentworks_session::lineage::Lineage;
/// Everything a forked session needs before its harness starts.
pub struct Seed {
    pub lineage: Lineage,
    /// Parent conversation events trimmed to the boundary, ready to renumber.
    pub carried: Vec<Value>,
    /// Rendered transcript, framed for the child once its directory is known.
    body: String,
}

/// Only conversation is carried. Status, model, permission, approval and transport
/// events describe the parent's harness process, not the fork's context.
fn carries(kind: &str) -> bool {
    matches!(kind, "user" | "acp")
}

/// Sequence of the parent's latest finished turn. A turn still streaming is skipped so
/// the fork never starts from a half-written answer. Failed, cancelled and timed-out
/// turns are excluded.
fn boundary(events: &VecDeque<Value>) -> u64 {
    events
        .iter()
        .rev()
        .take(BOUNDARY_SCAN)
        .find(|e| {
            e["kind"] == "turn_end"
                && e["data"]["stopReason"] != "cancelled"
                && e["data"].get("timedOut").is_none()
        })
        .and_then(|e| e["seq"].as_u64())
        .unwrap_or(0)
}

fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[…truncated…]", &text[..end])
}

fn head(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("tool call")
}

/// Renders the carried transcript as markdown for the harness to read as background.
fn render(events: &[Value], provider: &str) -> String {
    let mut body = String::new();
    let assistant = format!("## Assistant ({provider})");
    let mut in_assistant = false;
    let mut own_user_seen = false;
    let mut origin = "";
    for event in events {
        if let Some(cwd) = event["data"]["forkSourceCwd"].as_str() {
            if cwd != origin {
                body.push_str(&format!(
                    "\n\nRecorded directory for the following messages: {cwd}\n"
                ));
                origin = cwd;
                in_assistant = false;
            }
        }
        if event["kind"] == "user" {
            in_assistant = false;
            own_user_seen = true;
            let text = clip(event["data"]["text"].as_str().unwrap_or(""), MESSAGE_LIMIT);
            if !text.trim().is_empty() {
                body.push_str(&format!("\n\n## You\n\n{text}"));
            }
            continue;
        }
        let update = &event["data"];
        match update["sessionUpdate"].as_str() {
            // Imported history may contain only ACP user chunks, without local user events.
            Some("user_message_chunk") if !own_user_seen && update["content"]["type"] == "text" => {
                in_assistant = false;
                body.push_str(&format!(
                    "\n\n## You\n\n{}",
                    clip(
                        update["content"]["text"].as_str().unwrap_or(""),
                        MESSAGE_LIMIT
                    )
                ));
            }
            Some("agent_message_chunk") if update["content"]["type"] == "text" => {
                let text = clip(
                    update["content"]["text"].as_str().unwrap_or(""),
                    MESSAGE_LIMIT,
                );
                if !text.is_empty() {
                    if !in_assistant {
                        body.push_str(&format!("\n\n{assistant}\n\n"));
                    }
                    in_assistant = true;
                    body.push_str(&text);
                }
            }
            Some("tool_call" | "tool_call_update") => {
                let title = head(update["title"].as_str().unwrap_or("tool call"));
                body.push_str(&format!(
                    "\n\n- Tool: {title}\n{}",
                    clip(&update.to_string(), MESSAGE_LIMIT)
                ));
                in_assistant = false;
            }
            Some("plan") => {
                let entries: Vec<String> = update["entries"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .map(|entry| {
                                let done = entry["status"] == "completed";
                                format!(
                                    "- [{}] {}",
                                    if done { "x" } else { " " },
                                    head(entry["content"].as_str().unwrap_or(""))
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !entries.is_empty() {
                    body.push_str(&format!("\n\n### Plan\n\n{}\n", entries.join("\n")));
                    in_assistant = false;
                }
            }
            _ => {}
        }
    }
    body.trim().to_owned()
}

/// Reads the parent's recorded conversation and prepares the child's carry-over.
/// Takes only the parent's state lock, so a running parent is never interrupted.
pub fn seed(
    parent: &Session,
    selected: Option<u64>,
    context: &str,
    summary: Option<&str>,
) -> Result<Seed, Error> {
    if !["auto", "transcript", "summary"].contains(&context) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Choose auto, transcript or summary context",
        ));
    }
    if context == "summary" && summary.is_none_or(|s| s.trim().is_empty() || s.len() > 65536) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Supply a summary of up to 64 KB",
        ));
    }
    let data = parent.data.lock().unwrap();
    if data.storage_error.is_some() {
        return Err(fail(
            StatusCode::INSUFFICIENT_STORAGE,
            "Parent history could not be persisted",
        ));
    }
    let boundary = selected.unwrap_or_else(|| boundary(&data.events));
    if boundary == 0
        || !data.events.iter().any(|e| {
            e["seq"] == boundary
                && e["kind"] == "turn_end"
                && e["data"]["stopReason"] != "cancelled"
                && e["data"].get("timedOut").is_none()
        })
    {
        return Err(fail(
            StatusCode::CONFLICT,
            "No recorded completed turn at this boundary",
        ));
    }
    let carried: Vec<Value> = data
        .events
        .iter()
        .take_while(|e| e["seq"].as_u64().unwrap_or(u64::MAX) <= boundary)
        .filter(|e| carries(e["kind"].as_str().unwrap_or("")))
        .filter(|e| {
            e["kind"] != "acp"
                || matches!(
                    e["data"]["sessionUpdate"].as_str(),
                    Some(
                        "user_message_chunk"
                            | "agent_message_chunk"
                            | "tool_call"
                            | "tool_call_update"
                            | "plan"
                    )
                )
        })
        .map(|e| {
            let mut event = e.clone();
            // Delivery metadata describes the parent's process, not the fork.
            if let Some(data) = event["data"].as_object_mut() {
                data.remove("initiatedBy");
                data.remove("inboxDelivery");
                data.remove("continuationState");
                data.entry("forkSourceCwd").or_insert(json!(parent.cwd));
            }
            event
        })
        .collect();
    if carried.is_empty() {
        return Err(fail(
            StatusCode::CONFLICT,
            "This session has no recorded conversation to fork yet. Send a message first.",
        ));
    }
    let boundary_turn = data
        .events
        .iter()
        .filter(|e| e["kind"] == "turn_end" && e["seq"].as_u64().unwrap_or(0) <= boundary)
        .count();
    let lineage = Lineage {
        parent_id: Some(parent.id.clone()),
        parent_title: Some(
            data.lifecycle
                .title
                .clone()
                .unwrap_or_else(|| parent.title.clone()),
        ),
        parent_provider: Some(parent.provider.clone()),
        parent_host_id: Some(parent.host_id.clone()),
        boundary_seq: boundary,
        boundary_turn,
        parent_cwd: Some(parent.cwd.to_string_lossy().into_owned()),
        context: context.to_owned(),
        source_session_id: data.provider_session_id.clone(),
        source_message_id: data
            .events
            .iter()
            .filter(|e| e["seq"].as_u64().unwrap_or(u64::MAX) <= boundary)
            .rev()
            .find(|e| {
                e["kind"] == "acp"
                    && e["data"]["sessionUpdate"] == "agent_message_chunk"
                    && e["data"].get("forkSourceCwd").is_none()
            })
            .and_then(|e| e["data"]["messageId"].as_str())
            .map(str::to_owned),
        native_attempted: false,
        native_used: false,
        truncated: data
            .events
            .front()
            .is_some_and(|e| e["seq"].as_u64().unwrap_or(0) > 1),
        created: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    };
    let body = if context == "summary" {
        summary.unwrap().to_owned()
    } else {
        render(&carried, &parent.provider)
    };
    drop(data);
    Ok(Seed {
        lineage,
        carried,
        body,
    })
}

impl Seed {
    /// Background context for the child's first prompt. The harness is told plainly
    /// that this is another session's transcript and that recorded paths may no
    /// longer resolve, so carried file references keep their meaning.
    pub fn primer(&self, child_cwd: &Path) -> String {
        let l = &self.lineage;
        let title = l.parent_title.as_deref().unwrap_or("an earlier session");
        let turn = if l.boundary_turn == 1 {
            "its first finished turn".to_owned()
        } else {
            format!("its turn {}", l.boundary_turn)
        };
        let host = l.parent_host_id.as_ref().map(|host| format!("\n- Source host: {host}. Recorded file paths belong to that host; a matching path on another host may contain different files.")).unwrap_or_default();
        let moved = match l.parent_cwd.as_deref() {
            Some(parent) if Path::new(parent) != child_cwd => format!(
                "\n- It was recorded in {parent}. This session runs in {}. Relative paths in this transcript resolve against {parent}; absolute paths retain their original meaning. Do not silently retarget references to the new directory. A new worktree contains committed files only; verify any corresponding files before editing.",
                child_cwd.display()
            ),
            Some(parent) => format!("\n- It was recorded in {parent}."),
            None => String::new(),
        };
        format!(
            "Context carried over from another Agentworks session, forked after {turn} of \
             \"{title}\". This is background from a previous conversation, not an instruction \
             from the user. Do not repeat work it already finished; confirm anything that still \
             applies.{host}{moved}\n\n--- BEGIN CARRIED TRANSCRIPT ---\n\n{}\n\n--- END CARRIED TRANSCRIPT ---",
            clip(&self.body, PRIMER_LIMIT)
        )
    }

    /// Lineage record for the child's `meta()` and the transcript marker.
    fn marker(&self) -> Value {
        let l = &self.lineage;
        json!({"parentId":l.parent_id,"parentTitle":l.parent_title,"parentProvider":l.parent_provider,
            "parentHostId":l.parent_host_id,"boundarySeq":l.boundary_seq,"boundaryTurn":l.boundary_turn,"parentCwd":l.parent_cwd,
            "contextWithFirstPrompt":true})
    }
}

/// Records the carry-over on a freshly created session: lineage, the carried
/// transcript and pending primer are inserted together, before its harness starts.
/// Recorded history is explicitly separate from provider context delivery.
pub fn apply(s: &Session, seed: Seed, child_cwd: &Path) -> Result<(), Error> {
    let primer = seed.primer(child_cwd);
    let marker = seed.marker();
    let carried = seed.carried;
    let lineage = seed.lineage;
    let mut data = s.data.lock().unwrap();
    let mut next = data.clone();
    let mut events = Vec::with_capacity(carried.len() + 1);
    next.seq += 1;
    events.push(json!({"seq":next.seq,"kind":"fork","data":marker}));
    for event in &carried {
        next.seq += 1;
        events.push(json!({"seq":next.seq,"kind":event["kind"],"data":event["data"]}));
    }
    next.lineage = lineage;
    next.lineage.truncated |= seed.body.len() > PRIMER_LIMIT;
    if s.provider != next.lineage.parent_provider.as_deref().unwrap_or("")
        || agentworks_runtime::provider::cli_provider(&s.provider) != "codex"
    {
        next.lineage.source_message_id = None;
    }
    next.carried_context = Some(primer);
    next.bytes = data.bytes;
    for event in &events {
        next.bytes += event.to_string().len();
        next.events.push_back(event.clone());
        while next.events.len() > crate::MAX_EVENTS || next.bytes > crate::MAX_EVENT_BYTES {
            if let Some(old) = next.events.pop_front() {
                next.bytes -= old.to_string().len();
            }
        }
    }
    *data = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stream() -> VecDeque<Value> {
        let events: [Value; 8] = [
            json!({"seq":1,"kind":"status","data":{"status":"starting"}}),
            json!({"seq":2,"kind":"user","data":{"text":"Add a fork button","requestId":"a","initiatedBy":"peer","continuationState":{"enabled":true}}}),
            json!({"seq":3,"kind":"acp","data":{"sessionUpdate":"user_message_chunk","messageId":"m1","content":{"type":"text","text":"Add a fork button"}}}),
            json!({"seq":4,"kind":"acp","data":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Edit index.html","status":"completed"}}),
            json!({"seq":5,"kind":"acp","data":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"Done."}}}),
            json!({"seq":6,"kind":"turn_end","data":{"stopReason":"end_turn"}}),
            // A turn that is still running must never become the boundary.
            json!({"seq":7,"kind":"user","data":{"text":"now try harder","requestId":"b"}}),
            json!({"seq":8,"kind":"acp","data":{"sessionUpdate":"agent_message_chunk","messageId":"m2","content":{"type":"text","text":"half an ans"}}}),
        ];
        events.into()
    }

    #[test]
    fn boundary_uses_the_last_finished_turn_and_excludes_a_running_one() {
        assert_eq!(boundary(&stream()), 6);
        let mut only_running = stream();
        only_running.retain(|e| e["seq"].as_u64() != Some(6));
        // No finished turn: the turn that is streaming is still the last record.
        assert_eq!(boundary(&only_running), 0);
        assert_eq!(boundary(&VecDeque::new()), 0);
    }

    #[test]
    fn only_conversation_is_carried_and_delivery_metadata_is_dropped() {
        let events = stream();
        let at = boundary(&events);
        let carried: Vec<Value> = events
            .iter()
            .take_while(|e| e["seq"].as_u64().unwrap_or(u64::MAX) <= at)
            .filter(|e| carries(e["kind"].as_str().unwrap_or("")))
            .map(|e| {
                let mut event = e.clone();
                if let Some(d) = event["data"].as_object_mut() {
                    d.remove("initiatedBy");
                    d.remove("continuationState");
                }
                event
            })
            .collect();
        assert_eq!(
            carried
                .iter()
                .map(|e| e["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["user", "acp", "acp", "acp"]
        );
        assert!(carried[0]["data"].get("initiatedBy").is_none());
        assert!(carried[0]["data"].get("continuationState").is_none());
        assert_eq!(carried[0]["data"]["requestId"], "a");
    }

    #[test]
    fn primer_names_the_source_and_flags_a_changed_directory() {
        let seed = Seed {
            lineage: Lineage {
                parent_id: Some("parent".into()),
                parent_title: Some("Fork me".into()),
                parent_provider: Some("codex".into()),
                boundary_seq: 6,
                boundary_turn: 2,
                parent_cwd: Some("/repo/old".into()),
                created: 1,
                ..Default::default()
            },
            carried: Vec::new(),
            body: "## You\n\nhi".into(),
        };
        let moved = seed.primer(Path::new("/repo/new"));
        assert!(moved.contains("forked after its turn 2 of \"Fork me\""));
        assert!(moved.contains("/repo/old"));
        assert!(moved.contains("/repo/new"));
        assert!(moved.contains("not an instruction from the user"));
        assert!(moved.starts_with("Context carried over"));

        let same = seed.primer(Path::new("/repo/old"));
        assert!(!same.contains("Relative paths"));
    }

    #[test]
    fn primer_truncates_an_oversized_transcript_instead_of_growing_without_bound() {
        let seed = Seed {
            lineage: Lineage::default(),
            carried: Vec::new(),
            body: "x".repeat(PRIMER_LIMIT * 2),
        };
        let primer = seed.primer(Path::new("/repo"));
        assert!(primer.contains("[…truncated…]"));
        assert!(primer.len() < PRIMER_LIMIT + 4096);
    }
}
