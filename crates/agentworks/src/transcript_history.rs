//! Older transcript pages behind a bounded session snapshot. The snapshot keeps only
//! the recent event window; every stored event row stays in the database, so earlier
//! history is paged straight from it with an exclusive `before` sequence cursor.
//! Reading never changes retention.
use crate::{ApiResult, App, fail, session};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Events that only set the session's current settings. A snapshot carries the
/// current values themselves (`models`, `permissionModes`, `sessionConfig`,
/// `commands`), applied after its events, so in a snapshot these change nothing.
const SETTINGS: [&str; 4] = ["models", "permission_modes", "session_config", "commands"];

fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}
fn nonneg(v: &Value) -> Option<f64> {
    number(v).filter(|n| *n >= 0.0)
}

/// What a `usage_update` sets in the composer's usage state, mirroring
/// `applyUsageEvent` in web/usage-status.js; `None` when it is attributed to a
/// subagent or parent tool call and only counted.
fn usage_cells(d: &Value) -> Option<Vec<String>> {
    let m = &d["_meta"];
    let attributed = !d["parentSessionId"].is_null()
        || [
            "parentToolUseId",
            "parent_tool_use_id",
            "_claude/parentToolUseId",
            "subagent",
            "_claude/subagent",
            "agentId",
        ]
        .iter()
        .any(|k| !d[*k].is_null() || !m[*k].is_null());
    if attributed {
        return None;
    }
    let mut cells = Vec::new();
    if nonneg(&d["used"]).is_some() || nonneg(&d["size"]).is_some_and(|s| s > 0.0) {
        cells.push("context".to_owned());
    }
    if nonneg(&d["cost"]["amount"]).is_some() {
        cells.push("cost".to_owned());
    }
    if m["_claude/model"].as_str().is_some_and(|s| !s.is_empty()) {
        cells.push("model".to_owned());
    }
    for key in ["five_hour", "seven_day"] {
        let u = number(&m["_claude/rateLimit"]["unifiedWindows"][key]["utilization"]);
        if u.is_some_and(|u| (0.0..=1.0).contains(&u)) {
            cells.push(format!("quota:{key}"));
        }
    }
    let codex = &m["_codex/subscriptionLimits"];
    if codex["version"] == 1
        && let Some(limits) = codex["limits"].as_array()
    {
        const FIELDS: [&str; 4] = ["limitName", "usedPercent", "windowDurationMins", "resetsAt"];
        for limit in limits {
            let (Some(id), Some(fields)) = (limit["limitId"].as_str(), limit.as_object()) else {
                continue;
            };
            for role in ["primary", "secondary"] {
                let Some(raw) = fields.get(role) else {
                    continue;
                };
                let cell = |f: &str| serde_json::to_string(&(id, role, f)).unwrap_or_default();
                if raw.is_null() {
                    // Clearing a window drops every field of it.
                    cells.extend(FIELDS.map(cell));
                } else if let Some(raw) = raw.as_object() {
                    if fields.contains_key("limitName") {
                        cells.push(cell("limitName"));
                    }
                    cells.extend(
                        FIELDS[1..]
                            .iter()
                            .filter(|f| raw.contains_key(**f))
                            .map(|f| cell(f)),
                    );
                }
            }
        }
    }
    Some(cells)
}

/// Drops events a full snapshot makes redundant: settings events, and usage
/// updates whose every value a later one replaces. The context reading before the
/// newest is kept, since the newest is compared with it.
pub fn without_superseded(events: Vec<Value>) -> Vec<Value> {
    let mut covered = std::collections::HashSet::new();
    let mut contexts = 0;
    let mut keep = vec![true; events.len()];
    for (i, e) in events.iter().enumerate().rev() {
        if SETTINGS.contains(&e["kind"].as_str().unwrap_or("")) {
            keep[i] = false;
            continue;
        }
        if e["kind"] != "acp" || e["data"]["sessionUpdate"] != "usage_update" {
            continue;
        }
        let Some(cells) = usage_cells(&e["data"]) else {
            continue;
        };
        let context = cells.iter().any(|c| c == "context");
        let new = cells.iter().any(|c| !covered.contains(c));
        keep[i] = new || (context && contexts == 1);
        contexts += usize::from(context);
        covered.extend(cells);
    }
    events
        .into_iter()
        .zip(keep)
        .filter_map(|(e, keep)| keep.then_some(e))
        .collect()
}

/// A full browser snapshot opens on recent turns only; earlier ones are paged on demand.
const OPEN_EVENTS: usize = 1000;
const OPEN_BYTES: usize = 512 * 1024;

/// The newest browser events within the opening budget, starting at a turn's prompt
/// when one falls inside it. `true` when earlier events were left out.
pub fn opening(mut events: Vec<Value>) -> (Vec<Value>, bool) {
    let (mut bytes, mut start) = (0, events.len());
    while start > 0 && events.len() - start < OPEN_EVENTS {
        bytes += events[start - 1].to_string().len();
        if bytes > OPEN_BYTES && start < events.len() {
            break;
        }
        start -= 1;
    }
    if start == 0 {
        return (events, false);
    }
    if let Some(turn) = events[start..].iter().position(|e| e["kind"] == "user") {
        start += turn;
    }
    (events.split_off(start), true)
}

const DEFAULT_PAGE: usize = 500;
const MAX_PAGE: usize = 2000;
/// Byte budget per page, well under the 4 MiB snapshot window.
const MAX_PAGE_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    before: u64,
    limit: Option<usize>,
}

/// `events` are oldest first and all have seq < `before`. `earliestSeq` is the cursor for
/// the next page (null when nothing was returned); `hasMore: false` means the beginning
/// of the stored conversation has been reached.
pub async fn older(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> ApiResult {
    let limit = page.limit.unwrap_or(DEFAULT_PAGE);
    if page.before == 0 || !(1..=MAX_PAGE).contains(&limit) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            format!("before must be a positive sequence and limit 1–{MAX_PAGE}"),
        ));
    }
    let s = session(&app, &id)?;
    let (events, more) = s
        .store
        .lock()
        .unwrap()
        .older_events(&id, page.before, limit, MAX_PAGE_BYTES)
        .map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not read saved transcript history",
            )
        })?;
    let earliest = events.first().map(|e| e["seq"].clone());
    Ok(Json(
        json!({"before":page.before,"events":events,"hasMore":more,"earliestSeq":earliest}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(seq: u64, data: Value) -> Value {
        let mut data = data;
        data["sessionUpdate"] = json!("usage_update");
        json!({"seq":seq,"kind":"acp","data":data})
    }
    fn seqs(events: &[Value]) -> Vec<u64> {
        events.iter().map(|e| e["seq"].as_u64().unwrap()).collect()
    }

    #[test]
    fn superseded_usage_and_settings_events_are_dropped() {
        let codex = |used: u64| {
            json!({"_codex/subscriptionLimits":{"version":1,"limits":[{"limitId":"codex","limitName":null,
            "primary":{"usedPercent":{"value":used},"resetsAt":{"value":1}},"secondary":{"usedPercent":{"value":1}}}]}})
        };
        let events = vec![
            json!({"seq":1,"kind":"models","data":{}}),
            usage(2, json!({"used":10,"size":100,"cost":{"amount":1}})),
            usage(3, json!({"used":20,"size":100,"_meta":codex(5)})),
            json!({"seq":4,"kind":"user","data":{}}),
            usage(5, json!({"used":30,"size":100,"_meta":{"agentId":"sub"}})),
            usage(6, json!({"used":40,"size":100})),
            usage(7, json!({"used":50,"size":100,"_meta":codex(9)})),
            json!({"seq":8,"kind":"session_config","data":{}}),
        ];
        let kept = without_superseded(events);
        // 2 keeps the only cost; 3 is replaced by 7; 5 is attributed elsewhere and
        // counted; 6 is the context reading before the newest.
        assert_eq!(seqs(&kept), [2, 4, 5, 6, 7]);
    }

    #[test]
    fn a_cleared_window_or_partial_update_keeps_what_it_alone_sets() {
        let limit = |primary: Value| json!({"_codex/subscriptionLimits":{"version":1,"limits":[{"limitId":"codex","primary":primary}]}});
        let events = vec![
            usage(
                1,
                json!({"_meta":limit(json!({"usedPercent":{"value":1},"resetsAt":{"value":2}}))}),
            ),
            usage(
                2,
                json!({"_meta":limit(json!({"usedPercent":{"value":3}}))}),
            ),
        ];
        assert_eq!(
            seqs(&without_superseded(events)),
            [1, 2],
            "resetsAt is only in 1"
        );
        let events = vec![
            usage(
                1,
                json!({"_meta":limit(json!({"usedPercent":{"value":1}}))}),
            ),
            usage(2, json!({"_meta":limit(Value::Null)})),
        ];
        assert_eq!(seqs(&without_superseded(events)), [2]);
        // An event that sets nothing valid only counts.
        let events = vec![usage(1, json!({"used":-1})), usage(2, json!({"used":1}))];
        assert_eq!(seqs(&without_superseded(events)), [2]);
    }

    #[test]
    fn opening_keeps_recent_whole_turns() {
        let events: Vec<Value> = (1..=OPEN_EVENTS as u64 + 50)
            .map(|seq| json!({"seq":seq,"kind":if seq % 100 == 1 {"user"} else {"acp"}}))
            .collect();
        let (kept, cut) = opening(events.clone());
        assert!(cut);
        assert_eq!(kept[0]["kind"], "user", "starts at a prompt");
        assert_eq!(kept.last(), events.last());
        assert!(kept.len() <= OPEN_EVENTS);
        let (all, cut) = opening(events[..10].to_vec());
        assert!(!cut);
        assert_eq!(all.len(), 10);
        // One oversized event is still shown.
        let big = vec![json!({"seq":1,"kind":"acp","text":"x".repeat(OPEN_BYTES * 2)})];
        assert_eq!(opening(big).0.len(), 1);
    }

    /// Prunes a JSON array of events: `AW_EVENTS_IN=a.json AW_EVENTS_OUT=b.json cargo test -- --ignored prune_file`.
    #[test]
    #[ignore]
    fn prune_file() {
        let events: Vec<Value> =
            serde_json::from_slice(&std::fs::read(std::env::var("AW_EVENTS_IN").unwrap()).unwrap())
                .unwrap();
        let kept = without_superseded(events);
        std::fs::write(
            std::env::var("AW_EVENTS_OUT").unwrap(),
            serde_json::to_vec(&kept).unwrap(),
        )
        .unwrap();
    }
}
