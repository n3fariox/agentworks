//! L-02 questions/elicitation. Harnesses ask questions with ACP `elicitation/create`
//! (Claude's AskUserQuestion and MCP form elicitations, Codex MCP elicitations).
//! Pending questions share the approval map so cancellation, restarts, timeouts and
//! notifications treat them like approvals. This module owns form support checks,
//! authoritative answer validation and idempotent submission receipts.
use crate::Session;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::sync::Mutex;

const SCALARS: [&str; 4] = ["string", "number", "integer", "boolean"];
const RECEIPTS: usize = 256;

/// Why a request cannot be rendered as a form, in words a person can act on.
/// `None` means every field is a supported primitive.
pub fn unsupported(request: &Value) -> Option<String> {
    let mode = request["mode"].as_str().unwrap_or("unknown");
    if mode != "form" {
        return Some(format!(
            "The agent asked for a “{mode}” interaction, which Agentworks cannot show. Skip or cancel it."
        ));
    }
    let Some(properties) = request["requestedSchema"]["properties"].as_object() else {
        return Some("The question has no readable fields. Skip or cancel it.".into());
    };
    for (name, field) in properties {
        let kind = field["type"].as_str().unwrap_or("");
        let ok =
            SCALARS.contains(&kind) || (kind == "array" && !choices(&field["items"]).is_empty());
        if !ok {
            return Some(format!(
                "Field “{}” uses an unsupported input type ({}). Skip or cancel it.",
                label(name, field),
                if kind.is_empty() { "none" } else { kind }
            ));
        }
    }
    None
}

fn label<'a>(name: &'a str, field: &'a Value) -> &'a str {
    field["title"]
        .as_str()
        .filter(|t| !t.is_empty())
        .unwrap_or(name)
}

/// Allowed values from `enum`, `oneOf` or `anyOf` (`const`) lists.
fn choices(schema: &Value) -> Vec<&str> {
    if let Some(values) = schema["enum"].as_array() {
        return values.iter().filter_map(Value::as_str).collect();
    }
    ["oneOf", "anyOf"]
        .iter()
        .filter_map(|k| schema[*k].as_array())
        .flatten()
        .filter_map(|o| o["const"].as_str())
        .collect()
}

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

fn valid_date(s: &str) -> bool {
    let mut parts = s.split('-');
    let (Some(y), Some(m), Some(d), None) = (
        parts.next().and_then(|p| digits(p, 4)),
        parts.next().and_then(|p| digits(p, 2)),
        parts.next().and_then(|p| digits(p, 2)),
        parts.next(),
    ) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    (1..=12).contains(&m) && d >= 1 && d <= days[m as usize - 1]
}

/// RFC 3339 date-time: `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`.
fn valid_date_time(s: &str) -> bool {
    let Some((date, time)) = s.split_once(['T', 't']) else {
        return false;
    };
    let (clock, zone) = if let Some(c) = time.strip_suffix(['Z', 'z']) {
        (c, None)
    } else if let Some(i) = time.rfind(['+', '-']) {
        (&time[..i], Some(&time[i + 1..]))
    } else {
        return false;
    };
    let zone_ok = zone.is_none_or(|z| {
        matches!(z.split_once(':'), Some((h, m)) if digits(h,2).is_some_and(|h|h<24) && digits(m,2).is_some_and(|m|m<60))
    });
    let clock = clock.split_once('.').map_or(clock, |(c, frac)| {
        if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
            ""
        } else {
            c
        }
    });
    let mut hms = clock.split(':');
    let ok = matches!(
        (hms.next().and_then(|h| digits(h, 2)), hms.next().and_then(|m| digits(m, 2)), hms.next().and_then(|s| digits(s, 2)), hms.next()),
        (Some(h), Some(m), Some(s), None) if h < 24 && m < 60 && s <= 60
    );
    valid_date(date) && ok && zone_ok
}

fn valid_format(format: &str, s: &str) -> bool {
    match format {
        "email" => s.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
                && !s.chars().any(char::is_whitespace)
        }),
        "uri" => s.split_once(':').is_some_and(|(scheme, rest)| {
            scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
                && !rest.is_empty()
                && !s.chars().any(char::is_whitespace)
        }),
        "date" => valid_date(s),
        "date-time" => valid_date_time(s),
        _ => true,
    }
}

fn check(field: &Value, value: &Value) -> Result<(), String> {
    let bound = |key: &str| field[key].as_f64();
    match field["type"].as_str().unwrap_or("") {
        "string" => {
            let s = value.as_str().ok_or("must be text")?;
            let len = s.chars().count() as f64;
            if bound("minLength").is_some_and(|m| len < m) {
                return Err(format!(
                    "must be at least {} characters",
                    field["minLength"]
                ));
            }
            if bound("maxLength").is_some_and(|m| len > m) {
                return Err(format!("must be at most {} characters", field["maxLength"]));
            }
            let allowed = choices(field);
            if !allowed.is_empty() && !allowed.contains(&s) {
                return Err("is not one of the offered choices".into());
            }
            // `pattern` is ECMAScript regex syntax; the browser checks it and the harness
            // re-validates. Agentworks has no regex engine to enforce it here.
            if let Some(format) = field["format"].as_str()
                && !valid_format(format, s)
            {
                return Err(format!("must be a valid {format}"));
            }
        }
        kind @ ("number" | "integer") => {
            let n = value
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or("must be a number")?;
            if kind == "integer" && n.fract() != 0.0 {
                return Err("must be a whole number".into());
            }
            if bound("minimum").is_some_and(|m| n < m) {
                return Err(format!("must be at least {}", field["minimum"]));
            }
            if bound("maximum").is_some_and(|m| n > m) {
                return Err(format!("must be at most {}", field["maximum"]));
            }
        }
        "boolean" => {
            value.as_bool().ok_or("must be yes or no")?;
        }
        "array" => {
            let items = value.as_array().ok_or("must be a list of choices")?;
            let allowed = choices(&field["items"]);
            let mut seen = Vec::new();
            for item in items {
                let s = item.as_str().ok_or("must be a list of choices")?;
                if !allowed.contains(&s) {
                    return Err("contains a choice that was not offered".into());
                }
                if seen.contains(&s) {
                    return Err("contains a choice more than once".into());
                }
                seen.push(s);
            }
            let count = items.len() as f64;
            if bound("minItems").is_some_and(|m| count < m) {
                return Err(format!("needs at least {} selections", field["minItems"]));
            }
            if bound("maxItems").is_some_and(|m| count > m) {
                return Err(format!("allows at most {} selections", field["maxItems"]));
            }
        }
        _ => return Err("uses an unsupported input type".into()),
    }
    Ok(())
}

/// Validate accepted content against the requested schema. Errors are keyed by
/// field name (`""` for form-level problems) so the browser can place them.
pub fn validate(request: &Value, content: &Value) -> Result<(), Vec<(String, String)>> {
    let schema = &request["requestedSchema"];
    let empty = Map::new();
    let properties = schema["properties"].as_object().unwrap_or(&empty);
    let Some(content) = content.as_object() else {
        return Err(vec![("".into(), "Answer must be an object".into())]);
    };
    let mut errors = Vec::new();
    for key in content.keys().filter(|k| !properties.contains_key(*k)) {
        errors.push((
            key.clone(),
            format!("“{key}” is not a field of this question"),
        ));
    }
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for (name, field) in properties {
        let title = label(name, field);
        match content.get(name) {
            None | Some(Value::Null) => {
                if required.contains(&name.as_str()) {
                    errors.push((name.clone(), format!("{title} is required")));
                }
            }
            Some(Value::String(s)) if s.is_empty() && required.contains(&name.as_str()) => {
                errors.push((name.clone(), format!("{title} is required")))
            }
            Some(value) => {
                if let Err(e) = check(field, value) {
                    errors.push((name.clone(), format!("{title} {e}")));
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[derive(Deserialize)]
pub struct Answer {
    pub action: String,
    #[serde(default)]
    pub content: Option<Value>,
    /// Browser-generated key; a retry with the same key is acknowledged, not re-sent.
    #[serde(rename = "submissionId")]
    pub submission_id: String,
}

/// Build the ACP response body for an answer, or say why it cannot be sent.
pub fn response(request: &Value, unsupported: bool, answer: &Answer) -> Result<Value, String> {
    if answer.submission_id.is_empty() || answer.submission_id.len() > 128 {
        return Err("A submission ID is required".into());
    }
    match answer.action.as_str() {
        "decline" | "cancel" => Ok(json!({"action":answer.action})),
        "accept" if unsupported => {
            Err("This question form is not supported; skip or cancel it instead".into())
        }
        "accept" => {
            let content = answer.content.clone().unwrap_or_else(|| json!({}));
            validate(request, &content).map_err(|errors| {
                errors
                    .into_iter()
                    .map(|(_, m)| m)
                    .collect::<Vec<_>>()
                    .join("; ")
            })?;
            Ok(json!({"action":"accept","content":content}))
        }
        _ => Err("Action must be accept, decline or cancel".into()),
    }
}

struct Receipt {
    session: String,
    question: String,
    submission: String,
    action: String,
}
static RECEIPTS_LOG: Mutex<VecDeque<Receipt>> = Mutex::new(VecDeque::new());

pub fn remember(session: &str, question: &str, submission: &str, action: &str) {
    let mut log = RECEIPTS_LOG.lock().unwrap();
    if log.len() >= RECEIPTS {
        log.pop_front();
    }
    log.push_back(Receipt {
        session: session.into(),
        question: question.into(),
        submission: submission.into(),
        action: action.into(),
    });
}

/// The action already delivered for this exact submission, if any.
pub fn delivered(session: &str, question: &str, submission: &str) -> Option<String> {
    RECEIPTS_LOG
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.session == session && r.question == question && r.submission == submission)
        .map(|r| r.action.clone())
}

/// The agent withdrew its question (`$/cancel_request`). Mirrors `approve`'s status
/// handling so the browser drops the card and the turn returns to running.
pub fn withdraw(s: &Session, id: &str) {
    let removed = {
        let mut approvals = s.approvals.lock().unwrap();
        let removed = approvals.remove(id).is_some();
        let mut data = s.data.lock().unwrap();
        let running =
            removed && approvals.is_empty() && data.turn_active && data.status == "waiting";
        if running {
            data.status = "running".into();
        }
        removed.then_some(running)
    };
    if let Some(running) = removed {
        s.emit(
            "approval_resolved",
            json!({"id":id,"cancelled":true,"withdrawn":true}),
        );
        if running {
            s.emit("status", json!({"status":"running"}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> Value {
        json!({"mode":"form","sessionId":"s","message":"Pick","requestedSchema":{"type":"object",
            "properties":{
                "question_0":{"type":"string","title":"Database","oneOf":[{"const":"Postgres","title":"Postgres"},{"const":"SQLite","title":"SQLite"}]},
                "question_0_custom":{"type":"string","title":"Other","maxLength":20},
                "tags":{"type":"array","items":{"anyOf":[{"const":"a","title":"A"},{"const":"b","title":"B"}]},"maxItems":1},
                "email":{"type":"string","format":"email"},
                "when":{"type":"string","format":"date-time"},
                "count":{"type":"integer","minimum":1,"maximum":5},
                "ratio":{"type":"number","maximum":1.5},
                "ok":{"type":"boolean"}},
            "required":["question_0","count"]}})
    }

    #[test]
    fn supported_and_unsupported_forms() {
        assert_eq!(unsupported(&ask()), None);
        assert!(
            unsupported(&json!({"mode":"url","url":"https://x"}))
                .unwrap()
                .contains("“url”")
        );
        let nested = json!({"mode":"form","requestedSchema":{"properties":{"o":{"type":"object","title":"Owner"}}}});
        assert!(unsupported(&nested).unwrap().contains("Owner"));
        let free_array = json!({"mode":"form","requestedSchema":{"properties":{"o":{"type":"array","items":{"type":"string"}}}}});
        assert!(unsupported(&free_array).is_some());
    }

    #[test]
    fn validates_every_field_kind() {
        let r = ask();
        assert!(validate(&r, &json!({"question_0":"SQLite","count":3})).is_ok());
        let full = json!({"question_0":"Postgres","question_0_custom":"note","tags":["a"],"email":"a@b.io",
            "when":"2026-10-20T10:00:00.5+02:00","count":5,"ratio":1.5,"ok":false});
        assert_eq!(validate(&r, &full), Ok(()));
        let errors = validate(&r, &json!({"question_0":"MySQL","question_0_custom":"x".repeat(21),
            "tags":["a","b"],"email":"nope","when":"2026-02-30T10:00:00Z","count":2.5,"ratio":2,"ok":"yes","extra":1}))
            .unwrap_err();
        let fields: Vec<_> = errors.iter().map(|(f, _)| f.as_str()).collect();
        for f in [
            "extra",
            "question_0",
            "question_0_custom",
            "tags",
            "email",
            "when",
            "count",
            "ratio",
            "ok",
        ] {
            assert!(fields.contains(&f), "{f} missing from {errors:?}");
        }
        let missing = validate(&r, &json!({"question_0":""})).unwrap_err();
        assert_eq!(missing.len(), 2);
        assert!(missing[0].1.contains("required"));
        assert!(
            validate(
                &r,
                &json!({"question_0":"SQLite","count":1,"tags":["a","a"]})
            )
            .is_err()
        );
        assert!(validate(&r, &json!([])).is_err());
    }

    #[test]
    fn formats() {
        assert!(valid_date("2024-02-29") && !valid_date("2023-02-29") && !valid_date("2024-2-01"));
        assert!(
            valid_date_time("2026-10-20T23:59:59Z") && valid_date_time("2026-10-20T00:00:00-05:30")
        );
        assert!(!valid_date_time("2026-10-20T10:00") && !valid_date_time("2026-10-20 10:00:00Z"));
        assert!(
            valid_format("uri", "https://example.com/x") && !valid_format("uri", "example com")
        );
        assert!(!valid_format("email", "a@b") && !valid_format("email", "a b@c.d"));
    }

    #[test]
    fn responses_and_receipts() {
        let r = ask();
        let answer = |action: &str, content: Value| Answer {
            action: action.into(),
            content: Some(content),
            submission_id: "sub".into(),
        };
        assert_eq!(
            response(&r, false, &answer("decline", json!(null))).unwrap(),
            json!({"action":"decline"})
        );
        assert!(
            response(&r, true, &answer("accept", json!({})))
                .unwrap_err()
                .contains("not supported")
        );
        assert!(
            response(&r, false, &answer("accept", json!({"count":1})))
                .unwrap_err()
                .contains("Database is required")
        );
        assert!(response(&r, false, &answer("maybe", json!({}))).is_err());
        let ok = response(
            &r,
            false,
            &answer("accept", json!({"question_0":"SQLite","count":1})),
        )
        .unwrap();
        assert_eq!(ok["content"]["question_0"], "SQLite");
        assert!(
            response(
                &r,
                false,
                &Answer {
                    action: "cancel".into(),
                    content: None,
                    submission_id: String::new()
                }
            )
            .is_err()
        );
        remember("s1", "q1", "sub-a", "accept");
        assert_eq!(delivered("s1", "q1", "sub-a").as_deref(), Some("accept"));
        assert_eq!(delivered("s1", "q1", "sub-b"), None);
        assert_eq!(delivered("s2", "q1", "sub-a"), None);
    }
}
