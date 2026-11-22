//! Compacts closed transcript history in storage without changing what it renders.
//!
//! Harnesses stream a message as many tiny chunk events and a tool as many
//! snapshot updates. Once a turn has ended, both are folded:
//!
//! - A run of consecutive text chunks that agree on every field but the text
//!   becomes one event. It keeps the first chunk's `time`, takes the last
//!   chunk's `seq`, and records the first chunk's sequence as `from`. A browser
//!   that saw only part of the run cannot resume past it (`splits`); it takes a
//!   full snapshot instead, so no text is repeated or lost.
//! - A finished tool's updates become its final update, folded the way the
//!   browser applies them: later fields replace earlier ones, and terminal
//!   deltas become the whole `terminal_output`. Codex's `aggregatedOutput`
//!   repeats the tool's content text exactly and is dropped when it does.
//!
//! Sequence numbers are never reused, so stored history has gaps afterwards.
//! A per-session cursor records how far history has been compacted. Startup
//! compacts what is left over and vacuums weekly so the freed space returns to disk.
use crate::Session;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The compaction cursor per session, and when storage maintenance (vacuum) last ran.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS event_compaction(session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,through INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS maintenance(name TEXT PRIMARY KEY,at INTEGER NOT NULL);";

/// Stored rows read and rewritten per lock hold.
const BATCH: usize = 5000;
const CHUNKS: [&str; 3] = [
    "agent_message_chunk",
    "agent_thought_chunk",
    "user_message_chunk",
];

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// The first sequence an event stands for: `from` on a merged chunk run.
pub fn first_seq(event: &Value) -> u64 {
    event["from"]
        .as_u64()
        .or_else(|| event["seq"].as_u64())
        .unwrap_or(0)
}

/// Whether a browser that applied events through `after` saw only part of a merged event.
pub fn splits<'a>(events: impl IntoIterator<Item = &'a Value>, after: u64) -> bool {
    events.into_iter().any(|e| {
        e["from"]
            .as_u64()
            .is_some_and(|from| from <= after && e["seq"].as_u64().unwrap_or(0) > after)
    })
}

#[derive(Default, Debug)]
pub struct Plan {
    /// Sequences whose rows are deleted.
    pub remove: Vec<u64>,
    /// Events that replace the stored row with the same sequence.
    pub replace: Vec<Value>,
}
impl Plan {
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.replace.is_empty()
    }
}

/// Everything in a text chunk except its text, when the event is a plain text chunk.
fn chunk_key(event: &Value) -> Option<Map<String, Value>> {
    let envelope = event.as_object()?;
    if event["kind"] != "acp"
        || envelope
            .keys()
            .any(|k| !matches!(k.as_str(), "seq" | "kind" | "time" | "data" | "from"))
    {
        return None;
    }
    let data = envelope.get("data")?.as_object()?;
    if !CHUNKS.contains(&data.get("sessionUpdate")?.as_str()?) {
        return None;
    }
    let content = data.get("content")?.as_object()?;
    if content.len() != 2 || content.get("type")? != "text" || !content.get("text")?.is_string() {
        return None;
    }
    let mut key = data.clone();
    key.remove("content");
    Some(key)
}

fn merge_run(run: &[&Value], plan: &mut Plan) {
    let [first, .., last] = run else { return };
    let text: String = run
        .iter()
        .map(|e| e["data"]["content"]["text"].as_str().unwrap_or(""))
        .collect();
    let mut merged = (*first).clone();
    merged["seq"] = last["seq"].clone();
    merged["from"] = json!(first_seq(first));
    merged["data"]["content"]["text"] = json!(text);
    plan.remove.extend(
        run[..run.len() - 1]
            .iter()
            .filter_map(|e| e["seq"].as_u64()),
    );
    plan.replace.push(merged);
}

/// The browser's `toolOutputText`: text content blocks joined by newlines.
fn content_text(content: &Value) -> Option<String> {
    let texts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|c| c["type"] == "content" && c["content"]["type"] == "text")
        .filter_map(|c| c["content"]["text"].as_str())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// Drops Codex's `rawOutput.aggregatedOutput` when it repeats the content text exactly.
fn drop_repeated_output(data: &mut Value) -> bool {
    let Some(text) = content_text(&data["content"]) else {
        return false;
    };
    // `get_mut`, not indexing: indexing would insert a `rawOutput: null` the browser applies.
    let Some(raw) = data.get_mut("rawOutput").and_then(Value::as_object_mut) else {
        return false;
    };
    if raw.get("aggregatedOutput").and_then(Value::as_str) == Some(text.as_str()) {
        raw.remove("aggregatedOutput");
        return true;
    }
    false
}

/// One terminal stream, as the browser accumulates it for a tool.
#[derive(Default)]
struct Terminal {
    stream: Option<(String, String)>,
    exit: Option<Value>,
    /// The stream could not be reconstructed exactly (unknown prefix, several terminals).
    unknown: bool,
}
impl Terminal {
    fn observe(&mut self, meta: &Value, prefix_known: bool) {
        for (key, delta) in [("terminal_output", false), ("terminal_output_delta", true)] {
            let value = &meta[key];
            let (Some(id), Some(data)) = (value["terminal_id"].as_str(), value["data"].as_str())
            else {
                continue;
            };
            match &mut self.stream {
                Some((current, text)) if current == id => {
                    if delta {
                        text.push_str(data)
                    } else {
                        *text = data.to_owned()
                    }
                }
                Some(_) => self.unknown = true,
                None if delta && !prefix_known => self.unknown = true,
                None => self.stream = Some((id.to_owned(), data.to_owned())),
            }
        }
        if meta["terminal_exit"]["terminal_id"].is_string() {
            self.exit = Some(meta["terminal_exit"].clone());
        }
    }
}

fn is_tool(event: &Value, update: &str) -> bool {
    event["kind"] == "acp" && event["data"]["sessionUpdate"] == update
}

/// Folds each finished tool's updates into its last one.
fn fold_tools(events: &[Value], plan: &mut Plan) {
    let mut calls: HashMap<String, &Value> = HashMap::new();
    let mut updates: HashMap<String, Vec<&Value>> = HashMap::new();
    let mut order = Vec::new();
    for event in events {
        let Some(key) = agentworks_presentation::tool_call::key(&event["data"]) else {
            continue;
        };
        if is_tool(event, "tool_call") {
            calls.entry(key).or_insert(event);
        } else if is_tool(event, "tool_call_update") {
            let list = updates.entry(key.clone()).or_default();
            if list.is_empty() {
                order.push(key);
            }
            list.push(event);
        }
    }
    for key in order {
        let list = &updates[&key];
        let finished = list
            .iter()
            .rev()
            .find_map(|e| e["data"]["status"].as_str())
            .is_some_and(|s| matches!(s, "completed" | "failed"));
        if !finished {
            continue;
        }
        let call = calls.get(&key);
        let mut terminal = Terminal::default();
        if let Some(call) = call {
            terminal.observe(&call["data"]["_meta"], true);
        }
        let mut fields = Map::new();
        let mut meta: Option<Value> = None;
        let mut streamed = false;
        for update in list {
            let data = update["data"].as_object().cloned().unwrap_or_default();
            if let Some(m) = data.get("_meta") {
                streamed |= m["terminal_output"].is_object()
                    || m["terminal_output_delta"].is_object()
                    || m["terminal_exit"].is_object();
                terminal.observe(m, call.is_some());
                // `Object.assign` keeps the last `_meta` whole.
                meta = Some(m.clone());
            }
            fields.extend(data.into_iter().filter(|(k, _)| k != "_meta"));
        }
        if terminal.unknown {
            continue;
        }
        if let Some(mut m) = meta {
            if let Some(object) = m.as_object_mut() {
                object.remove("terminal_output_delta");
                object.remove("terminal_output");
                object.remove("terminal_exit");
                if streamed {
                    if let Some((id, data)) = terminal.stream {
                        object.insert(
                            "terminal_output".into(),
                            json!({"terminal_id":id,"data":data}),
                        );
                    }
                    if let Some(exit) = terminal.exit {
                        object.insert("terminal_exit".into(), exit);
                    }
                }
            }
            fields.insert("_meta".into(), m);
        }
        let last = *list.last().unwrap();
        let mut data = Value::Object(fields);
        let deduplicated = drop_repeated_output(&mut data);
        if list.len() == 1 && !deduplicated {
            continue;
        }
        let mut merged = last.clone();
        merged["data"] = data;
        plan.remove.extend(
            list[..list.len() - 1]
                .iter()
                .filter_map(|e| e["seq"].as_u64()),
        );
        plan.replace.push(merged);
    }
}

/// The rewrite of a closed, contiguous range of stored events, oldest first.
pub fn plan(events: &[Value]) -> Plan {
    let mut plan = Plan::default();
    let mut run: Vec<&Value> = Vec::new();
    let mut run_key = None;
    for event in events {
        let key = chunk_key(event);
        if key.is_some() && key == run_key {
            run.push(event);
            continue;
        }
        merge_run(&run, &mut plan);
        run.clear();
        if key.is_some() {
            run.push(event);
        }
        run_key = key;
    }
    merge_run(&run, &mut plan);
    fold_tools(events, &mut plan);
    plan
}

#[derive(Default, Debug, Clone, Copy)]
pub struct Stats {
    pub removed: usize,
    pub rewritten: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
}
impl std::ops::AddAssign for Stats {
    fn add_assign(&mut self, o: Self) {
        self.removed += o.removed;
        self.rewritten += o.rewritten;
        self.bytes_before += o.bytes_before;
        self.bytes_after += o.bytes_after;
    }
}

/// What one batch did, and whether stored events past it remain to be compacted.
pub struct Batch {
    pub stats: Stats,
    pub more: bool,
}

/// Compacts the next batch of a session's stored events after its cursor, up to `through`.
pub fn compact_batch(store: &mut crate::store::Store, id: &str, through: u64) -> Result<Batch> {
    let mut stats = Stats::default();
    let cursor = store.compaction_cursor(id)?;
    if cursor >= through {
        return Ok(Batch { stats, more: false });
    }
    let rows = store.events_between(id, cursor, through, BATCH)?;
    let full = rows.len() == BATCH;
    let mut events = rows
        .iter()
        .map(|r| serde_json::from_str::<Value>(r))
        .collect::<serde_json::Result<Vec<Value>>>()?;
    // A partial batch ends after its last turn so runs and tools stay whole.
    let mut end = through;
    if full {
        let cut = events
            .iter()
            .rposition(|e| e["kind"] == "turn_end")
            .unwrap_or(events.len() - 1);
        events.truncate(cut + 1);
        end = events
            .last()
            .and_then(|e| e["seq"].as_u64())
            .unwrap_or(through);
    }
    let plan = plan(&events);
    if !plan.is_empty() {
        let sizes: HashMap<u64, usize> = events
            .iter()
            .zip(&rows)
            .filter_map(|(e, r)| Some((e["seq"].as_u64()?, r.len())))
            .collect();
        stats.removed = plan.remove.len();
        stats.rewritten = plan.replace.len();
        stats.bytes_before = plan
            .remove
            .iter()
            .copied()
            .chain(plan.replace.iter().filter_map(|e| e["seq"].as_u64()))
            .filter_map(|seq| sizes.get(&seq))
            .sum();
        stats.bytes_after = plan.replace.iter().map(|e| e.to_string().len()).sum();
    }
    store.apply_compaction(id, &plan, end)?;
    Ok(Batch { stats, more: full })
}

/// Compacts the session's stored events up to `through`, in batches that hold
/// the session and store locks briefly. A live window is reloaded from storage.
pub fn compact_session(s: &Session, through: u64) -> Result<Stats> {
    let mut total = Stats::default();
    loop {
        let mut data = s.data.lock().unwrap();
        // Unsaved events would make storage disagree with the window.
        if data.storage_error.is_some() {
            return Ok(total);
        }
        let mut store = s.store.lock().unwrap();
        let batch = compact_batch(&mut store, &s.id, through)?;
        if batch.stats.removed + batch.stats.rewritten > 0 && !data.parked {
            let recent = store.recent(&s.id)?;
            drop(store);
            let (events, bytes) = crate::store::window(recent)?;
            data.events = events;
            data.bytes = bytes;
        }
        drop(data);
        total += batch.stats;
        if !batch.more {
            return Ok(total);
        }
        // Let the session's own writers in between batches.
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Compacts the turn that just ended, off the session's task. Called right after
/// its `turn_end` (or error), while `turn_active` is still set: everything stored
/// so far belongs to a closed turn.
pub fn schedule(s: &std::sync::Arc<Session>) {
    let through = s.data.lock().unwrap().seq;
    let s = s.clone();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn_blocking(move || {
            if let Err(error) = compact_session(&s, through) {
                tracing::warn!(session = %s.id, "Transcript compaction failed: {error}");
            }
        });
    }
}

/// How often startup rewrites the database file to return space freed by compaction.
const VACUUM_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// A database with less free space than this is not worth rewriting.
const VACUUM_MIN_FREE: f64 = 0.01;

/// Startup maintenance, before any session is restored: no harness is running, so
/// all stored history is closed. Compacts history recorded before compaction
/// existed and turns that ended while the server stopped, then vacuums weekly.
pub fn startup(store: &mut crate::store::Store) {
    let started = Instant::now();
    let mut total = Stats::default();
    let sessions = match store.last_sequences() {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::warn!("Transcript compaction skipped: {error}");
            vec![]
        }
    };
    for (id, through) in sessions {
        loop {
            match compact_batch(store, &id, through) {
                Ok(batch) => {
                    total += batch.stats;
                    if !batch.more {
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!(session = %id, "Transcript compaction failed: {error}");
                    break;
                }
            }
        }
    }
    if total.removed + total.rewritten > 0 {
        tracing::info!(
            removed = total.removed,
            rewritten = total.rewritten,
            saved_bytes = total.bytes_before.saturating_sub(total.bytes_after),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Compacted stored transcripts"
        );
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if let Err(error) = vacuum_if_due(store, now) {
        tracing::warn!("Database vacuum failed: {error}");
    }
}

/// Vacuums when the last vacuum is a week old (or never ran) and there is space to return.
fn vacuum_if_due(store: &mut crate::store::Store, now: u64) -> Result<bool> {
    let last = store.maintained("vacuum")?;
    if last.is_some_and(|at| now.saturating_sub(at) < VACUUM_INTERVAL.as_millis() as u64) {
        return Ok(false);
    }
    let (pages, free, page_size) = store.pages()?;
    if pages == 0 || (free as f64) < pages as f64 * VACUUM_MIN_FREE {
        store.record_maintenance("vacuum", now)?;
        return Ok(false);
    }
    let started = Instant::now();
    store.vacuum()?;
    store.record_maintenance("vacuum", now)?;
    let (after, _, _) = store.pages()?;
    tracing::info!(
        before_bytes = pages * page_size,
        after_bytes = after * page_size,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Vacuumed session database"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(seq: u64, kind: &str, message: &str, text: &str) -> Value {
        json!({"seq":seq,"kind":"acp","time":1000+seq,"data":{"sessionUpdate":kind,"content":{"type":"text","text":text},"messageId":message,"acpSessionId":"a"}})
    }
    fn tool(seq: u64, update: &str, data: Value) -> Value {
        let mut data = data;
        data["sessionUpdate"] = json!(update);
        data["toolCallId"] = json!("t");
        json!({"seq":seq,"kind":"acp","time":seq,"data":data})
    }
    /// Applies a plan the way the store does.
    fn apply(events: &[Value], plan: &Plan) -> Vec<Value> {
        let replaced: HashMap<u64, &Value> = plan
            .replace
            .iter()
            .map(|e| (e["seq"].as_u64().unwrap(), e))
            .collect();
        events
            .iter()
            .filter(|e| !plan.remove.contains(&e["seq"].as_u64().unwrap()))
            .map(|e| {
                replaced
                    .get(&e["seq"].as_u64().unwrap())
                    .map_or(e.clone(), |r| (*r).clone())
            })
            .collect()
    }

    #[test]
    fn chunk_runs_merge_into_their_last_sequence_and_keep_the_first_time() {
        let events = vec![
            json!({"seq":1,"kind":"user","data":{"text":"hi"}}),
            chunk(2, "agent_thought_chunk", "m", "think"),
            chunk(3, "agent_message_chunk", "m", "Hel"),
            chunk(4, "agent_message_chunk", "m", "lo"),
            chunk(5, "agent_message_chunk", "m", "!"),
            chunk(6, "agent_message_chunk", "n", "Next"),
            json!({"seq":7,"kind":"turn_end","data":{}}),
        ];
        let out = apply(&events, &plan(&events));
        let seqs: Vec<u64> = out.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, [1, 2, 5, 6, 7]);
        assert_eq!(out[2]["data"]["content"]["text"], "Hello!");
        assert_eq!(out[2]["from"], 3);
        assert_eq!(out[2]["time"], 1003);
        assert_eq!(out[2]["data"]["messageId"], "m");
        assert!(out[3].get("from").is_none(), "single chunks are unchanged");
    }

    #[test]
    fn runs_stop_at_any_other_event_or_differing_field() {
        let mut odd = chunk(3, "agent_message_chunk", "m", "b");
        odd["data"]["content"]["annotations"] = json!({"x":1});
        let mut forked = chunk(5, "agent_message_chunk", "m", "d");
        forked["data"]["forkSourceCwd"] = json!("/x");
        let events = vec![
            chunk(1, "agent_message_chunk", "m", "a"),
            json!({"seq":2,"kind":"acp","data":{"sessionUpdate":"usage_update","used":1}}),
            odd,
            chunk(4, "agent_message_chunk", "m", "c"),
            forked,
        ];
        assert!(plan(&events).is_empty());
    }

    #[test]
    fn merged_runs_merge_again_and_keep_their_original_start() {
        let events = vec![
            chunk(1, "agent_message_chunk", "m", "a"),
            chunk(2, "agent_message_chunk", "m", "b"),
        ];
        let once = apply(&events, &plan(&events));
        let more = [
            once.clone(),
            vec![chunk(3, "agent_message_chunk", "m", "c")],
        ]
        .concat();
        let twice = apply(&more, &plan(&more));
        assert_eq!(twice.len(), 1);
        assert_eq!(twice[0]["from"], 1);
        assert_eq!(twice[0]["data"]["content"]["text"], "abc");
    }

    #[test]
    fn a_partly_seen_run_cannot_be_resumed_past() {
        let merged = json!({"seq":9,"from":5,"kind":"acp","data":{}});
        assert!(splits([&merged], 5));
        assert!(splits([&merged], 8));
        assert!(!splits([&merged], 4));
        assert!(!splits([&merged], 9));
        assert_eq!(first_seq(&merged), 5);
    }

    #[test]
    fn finished_tools_fold_to_their_final_update_with_whole_terminal_output() {
        let events = vec![
            tool(
                1,
                "tool_call",
                json!({"title":"run","status":"pending","_meta":{"terminal_output":{"terminal_id":"x","data":"a"}}}),
            ),
            tool(
                2,
                "tool_call_update",
                json!({"status":"in_progress","_meta":{"terminal_output_delta":{"terminal_id":"x","data":"b"}}}),
            ),
            chunk(3, "agent_message_chunk", "m", "between"),
            tool(
                4,
                "tool_call_update",
                json!({"content":[{"type":"content","content":{"type":"text","text":"ab"}}],"_meta":{"terminal_output_delta":{"terminal_id":"x","data":"c"}}}),
            ),
            tool(
                5,
                "tool_call_update",
                json!({"status":"completed","rawOutput":{"exitCode":0,"aggregatedOutput":"ab"},"_meta":{"terminal_exit":{"terminal_id":"x","exit_code":0},"other":1}}),
            ),
        ];
        let out = apply(&events, &plan(&events));
        let seqs: Vec<u64> = out.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, [1, 3, 5]);
        let last = &out[2]["data"];
        assert_eq!(last["status"], "completed");
        assert_eq!(last["content"][0]["content"]["text"], "ab");
        assert_eq!(
            last["_meta"]["terminal_output"],
            json!({"terminal_id":"x","data":"abc"})
        );
        assert_eq!(last["_meta"]["terminal_exit"]["exit_code"], 0);
        assert_eq!(last["_meta"]["other"], 1);
        assert!(last["_meta"].get("terminal_output_delta").is_none());
        assert_eq!(
            last["rawOutput"],
            json!({"exitCode":0}),
            "repeated output is dropped"
        );
    }

    #[test]
    fn running_tools_and_unknown_terminal_prefixes_are_left_alone() {
        let running = vec![
            tool(1, "tool_call", json!({"status":"pending"})),
            tool(
                2,
                "tool_call_update",
                json!({"status":"in_progress","content":[]}),
            ),
            tool(3, "tool_call_update", json!({"content":[]})),
        ];
        assert!(plan(&running).is_empty());
        // Without the tool_call in range, an appended delta's prefix is unknown.
        let headless = vec![
            tool(
                2,
                "tool_call_update",
                json!({"_meta":{"terminal_output_delta":{"terminal_id":"x","data":"b"}}}),
            ),
            tool(3, "tool_call_update", json!({"status":"completed"})),
        ];
        assert!(plan(&headless).is_empty());
    }

    #[test]
    fn folding_never_adds_fields() {
        let events = vec![
            tool(
                1,
                "tool_call_update",
                json!({"_meta":{"claudeCode":{"toolName":"Bash"}}}),
            ),
            tool(2, "tool_call_update", json!({"status":"failed"})),
        ];
        let out = apply(&events, &plan(&events));
        assert_eq!(out.len(), 1);
        let mut fields: Vec<&String> = out[0]["data"].as_object().unwrap().keys().collect();
        fields.sort();
        assert_eq!(fields, ["_meta", "sessionUpdate", "status", "toolCallId"]);
    }

    #[test]
    fn stored_history_compacts_once_and_keeps_its_cursor() {
        let dir = std::env::temp_dir().join(format!("aw-compact-{}", uuid::Uuid::new_v4()));
        let mut store = crate::store::Store::open(&dir.join("s.sqlite3")).unwrap();
        store
            .connection
            .execute_batch(
                "INSERT INTO sessions VALUES ('a','{}','{}');
                 INSERT INTO sessions VALUES ('b','{}','{}');",
            )
            .unwrap();
        let events = [
            chunk(1, "agent_message_chunk", "m", "a"),
            chunk(2, "agent_message_chunk", "m", "b"),
            json!({"seq":3,"kind":"turn_end","data":{}}),
            chunk(4, "agent_message_chunk", "n", "c"),
            chunk(5, "agent_message_chunk", "n", "d"),
        ];
        for id in ["a", "b"] {
            for e in &events {
                store
                    .connection
                    .execute(
                        "INSERT INTO events VALUES (?1,?2,?3)",
                        rusqlite::params![id, e["seq"].as_i64(), e.to_string()],
                    )
                    .unwrap();
            }
        }
        // Only the closed turn is compacted; the open one waits for its end.
        let batch = compact_batch(&mut store, "a", 3).unwrap();
        assert_eq!(
            (batch.stats.removed, batch.stats.rewritten, batch.more),
            (1, 1, false)
        );
        assert_eq!(store.compaction_cursor("a").unwrap(), 3);
        assert_eq!(store.compaction_cursor("b").unwrap(), 0);
        let recent = store.recent("a").unwrap();
        assert_eq!(recent.len(), 4);
        let batch = compact_batch(&mut store, "a", 5).unwrap();
        assert_eq!(batch.stats.removed, 1);
        let (window, _) = crate::store::window(store.recent("a").unwrap()).unwrap();
        let texts: Vec<_> = window
            .iter()
            .map(|e| (e["seq"].clone(), e["data"]["content"]["text"].clone()))
            .collect();
        assert_eq!(
            texts,
            [
                (json!(2), json!("ab")),
                (json!(3), Value::Null),
                (json!(5), json!("cd"))
            ]
        );
        // Paging older history does not overlap the merged rows.
        let (older, more) = store.older_events("a", 5, 10, usize::MAX).unwrap();
        assert_eq!(older.len(), 2);
        assert!(!more);
        assert!(compact_batch(&mut store, "a", 5).unwrap().stats.removed == 0);
        store.delete("a").unwrap();
        assert_eq!(store.compaction_cursor("a").unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Startup maintenance on a copy of a real database:
    /// `AW_COMPACT_DB=/path cargo test --release -- --ignored compact_copy --nocapture`.
    #[test]
    #[ignore]
    fn compact_copy() {
        let path = std::env::var("AW_COMPACT_DB").expect("AW_COMPACT_DB");
        let mut store = crate::store::Store::open(std::path::Path::new(&path)).unwrap();
        let before = store.pages().unwrap();
        let started = Instant::now();
        startup(&mut store);
        let after = store.pages().unwrap();
        eprintln!(
            "startup maintenance {:?}: {} MB ({} MB free) -> {} MB, vacuum recorded {:?}",
            started.elapsed(),
            (before.0 * before.2) >> 20,
            (before.1 * before.2) >> 20,
            (after.0 * after.2) >> 20,
            store.maintained("vacuum").unwrap()
        );
        let again = Instant::now();
        startup(&mut store);
        eprintln!("second startup {:?}", again.elapsed());
    }

    #[test]
    fn vacuum_runs_weekly_and_only_when_space_was_freed() {
        let dir = std::env::temp_dir().join(format!("aw-vacuum-{}", uuid::Uuid::new_v4()));
        let mut store = crate::store::Store::open(&dir.join("s.sqlite3")).unwrap();
        store
            .connection
            .execute_batch("INSERT INTO sessions VALUES ('a','{}','{}');")
            .unwrap();
        let day = 24 * 60 * 60 * 1000;
        // Nothing freed yet: no rewrite, but the check is recorded.
        assert!(!vacuum_if_due(&mut store, 10 * day).unwrap());
        assert_eq!(store.maintained("vacuum").unwrap(), Some(10 * day));
        for seq in 1..=2000 {
            store
                .connection
                .execute(
                    "INSERT INTO events VALUES ('a',?1,?2)",
                    rusqlite::params![seq, "x".repeat(500)],
                )
                .unwrap();
        }
        store.connection.execute("DELETE FROM events", []).unwrap();
        assert!(
            store.pages().unwrap().1 > 0,
            "deleted rows leave free pages"
        );
        assert!(
            !vacuum_if_due(&mut store, 16 * day).unwrap(),
            "within a week"
        );
        assert!(vacuum_if_due(&mut store, 17 * day).unwrap());
        assert_eq!(store.pages().unwrap().1, 0);
        assert_eq!(store.maintained("vacuum").unwrap(), Some(17 * day));
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn distinct_output_is_kept() {
        let events = vec![tool(
            1,
            "tool_call_update",
            json!({"status":"completed","content":[{"type":"content","content":{"type":"text","text":"ab"}}],"rawOutput":{"aggregatedOutput":"other"}}),
        )];
        assert!(plan(&events).is_empty());
    }
}
