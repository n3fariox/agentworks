//! Stored session badges. Existing hooks stage set/clear operations; the host
//! commits them for the session bound to that invocation after it succeeds.
use crate::{ApiResult, App, Session, SessionData, fail, session};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Badge {
    pub id: String,
    pub label: String,
    pub tone: String,
    pub tooltip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub source: String,
}
const GITHUB: &str = "@github";
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn valid_url(value: &str) -> bool {
    value.len() <= 2048
        && value.starts_with("https://")
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
        && reqwest::Url::parse(value).is_ok_and(|u| {
            u.scheme() == "https"
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
        })
}
fn validate(value: &Value, source: &str) -> Result<Vec<Badge>, String> {
    let mut badges: Vec<Badge> =
        serde_json::from_value(value.clone()).map_err(|_| "Invalid badge fields")?;
    if badges.len() > 16 {
        return Err("At most 16 badges per source".into());
    }
    for badge in &mut badges {
        if !valid_id(&badge.id)
            || badge.label.trim().is_empty()
            || badge.label.chars().count() > 32
            || badge.label.chars().any(char::is_control)
            || badge.tooltip.chars().count() > 256
            || badge.tooltip.chars().any(char::is_control)
            || !matches!(
                badge.tone.as_str(),
                "neutral" | "success" | "warning" | "danger" | "info"
            )
            || badge.url.as_deref().is_some_and(|u| !valid_url(u))
        {
            return Err("Invalid badge text, tone or URL".into());
        }
        badge.source = source.to_owned();
    }
    Ok(badges)
}
fn enabled(db: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut query=db.prepare("SELECT s.name,s.hash FROM scripts s LEFT JOIN script_order o ON s.name=o.name WHERE s.enabled=1 ORDER BY COALESCE(o.priority,0) DESC,s.name COLLATE BINARY")?;
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}
fn visible(data: &SessionData, db: &Connection) -> Vec<Badge> {
    let mut sources = enabled(db).unwrap_or_default();
    sources.push((GITHUB.into(), "host".into()));
    let mut ids = HashSet::new();
    let mut badges = Vec::new();
    for (source, hash) in sources {
        let Some(record) = data
            .script_badges
            .get(&source)
            .filter(|r| r["hash"] == hash)
        else {
            continue;
        };
        for mut badge in validate(&record["badges"], &source).unwrap_or_default() {
            if !ids.insert(badge.id.clone()) {
                continue;
            }
            if source == GITHUB
                && record["observedAt"]
                    .as_u64()
                    .is_none_or(|at| now().saturating_sub(at) > 120_000)
            {
                badge.tone = "neutral".into();
                badge.tooltip = format!(
                    "{} · Status out of date",
                    badge.tooltip.chars().take(220).collect::<String>()
                );
            }
            badges.push(badge);
            if badges.len() == 32 {
                return badges;
            }
        }
    }
    badges
}
fn payload(data: &SessionData, db: &Connection) -> Value {
    json!({"badges":visible(data,db),"badgesAt":now(),"badgeRevision":data.badge_revision})
}
/// Apply one successful callback atomically. The source/session are host bound,
/// and enabled/hash checks prevent an in-flight obsolete script from writing.
pub fn apply(s: &Session, source: &str, hash: &str, effects: &[Value]) -> Result<(), String> {
    if effects.is_empty() {
        return Ok(());
    }
    if effects.len() > 16 {
        return Err("At most 16 badge changes per hook".into());
    }
    let mut data = s.data.lock().unwrap();
    let mut store = s.store.lock().unwrap();
    let db = &store.connection;
    let current: Option<String> = db
        .query_row(
            "SELECT hash FROM scripts WHERE name=?1 AND enabled=1",
            [source],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "Could not check badge source")?;
    if current.as_deref() != Some(hash) {
        return Ok(());
    }
    let record = data.script_badges.get(source).filter(|r| r["hash"] == hash);
    let mut badges = record
        .map(|r| validate(&r["badges"], source))
        .transpose()?
        .unwrap_or_default();
    for effect in effects {
        let id = effect["id"]
            .as_str()
            .filter(|id| valid_id(id))
            .ok_or("Invalid badge id")?;
        match effect["op"].as_str() {
            Some("clear") => badges.retain(|b| b.id != id),
            Some("set") => {
                let mut row = effect.clone();
                row.as_object_mut()
                    .ok_or("Invalid badge operation")?
                    .remove("op");
                let badge = validate(&json!([row]), source)?.remove(0);
                if let Some(old) = badges.iter_mut().find(|b| b.id == id) {
                    *old = badge;
                } else {
                    badges.push(badge);
                }
            }
            _ => return Err("Unknown badge operation".into()),
        }
    }
    if badges.len() > 16 {
        return Err("At most 16 badges per script".into());
    }
    let previous = data.script_badges.clone();
    let active: HashMap<_, _> = enabled(db)
        .map_err(|_| "Could not check badge sources")?
        .into_iter()
        .collect();
    data.script_badges.retain(|name, record| {
        name == GITHUB || active.get(name).is_some_and(|h| record["hash"] == *h)
    });
    if badges.is_empty() {
        data.script_badges.remove(source);
    } else {
        data.script_badges
            .insert(source.into(), json!({"hash":hash,"badges":badges}));
    }
    if data.script_badges == previous {
        return Ok(());
    }
    let count: usize = data
        .script_badges
        .values()
        .map(|r| r["badges"].as_array().map_or(0, Vec::len))
        .sum();
    if count > 32 {
        data.script_badges = previous;
        return Err("At most 32 stored badges per session".into());
    }
    let old_revision = data.badge_revision;
    data.badge_revision = data.badge_revision.saturating_add(1);
    if store.save_state(&s.id, &data).is_err() {
        data.badge_revision = old_revision;
        data.script_badges = previous;
        return Err("Could not persist badge changes".into());
    }
    let event = payload(&data, &store.connection);
    drop(store);
    drop(data);
    let _ = s.tx.send(json!({"kind":"session_badges","data":event}));
    Ok(())
}
/// GitHub is a host producer using the same stored badge state, not a script
/// callback invoked by a badge read. A successful sync replaces its own badges.
fn sync_github(s: &Session, context: &Value) -> Result<(), String> {
    let mut data = s.data.lock().unwrap();
    let previous = data.script_badges.clone();
    if context["status"] == "ready" {
        let mut badges = Vec::new();
        let available = 32usize.saturating_sub(
            data.script_badges
                .iter()
                .filter(|(name, _)| name.as_str() != GITHUB)
                .map(|(_, r)| r["badges"].as_array().map_or(0, Vec::len))
                .sum(),
        );
        for pr in context["snapshot"]["resources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["kind"] == "pullRequest")
            .take(16.min(available))
        {
            let review = pr["reviewSummary"]["state"]
                .as_str()
                .or(pr["reviewDecision"].as_str())
                .unwrap_or("UNKNOWN");
            let ci = pr["checks"]["state"].as_str().unwrap_or("UNKNOWN");
            let (tone, meaning) = if pr["state"] == "MERGED" {
                ("neutral", "Merged")
            } else if pr["state"] == "CLOSED" {
                ("neutral", "Closed")
            } else if pr["draft"] == true {
                ("neutral", "Draft")
            } else if review == "APPROVED" {
                ("success", "Approved")
            } else if matches!(review, "REVIEW_REQUIRED" | "CHANGES_REQUESTED" | "NONE")
                && matches!(ci, "FAILURE" | "ERROR")
            {
                ("danger", "Not approved; CI failing")
            } else if review == "CHANGES_REQUESTED" {
                ("warning", "Changes requested")
            } else if review == "REVIEW_REQUIRED" {
                ("warning", "Awaiting review")
            } else {
                ("neutral", "Review status unknown")
            };
            badges.push(Badge {
                id: format!("github.pr.{}", pr["number"]),
                label: format!("PR#{}", pr["number"]),
                tone: tone.into(),
                tooltip: format!(
                    "PR#{} · {meaning} · CI: {ci}{}",
                    pr["number"],
                    if pr["headMatches"] == false {
                        " · PR head differs from checkout"
                    } else {
                        ""
                    }
                ),
                url: pr["url"]
                    .as_str()
                    .filter(|u| valid_url(u))
                    .map(str::to_owned),
                source: GITHUB.into(),
            });
        }
        if badges.is_empty() {
            data.script_badges.remove(GITHUB);
        } else {
            data.script_badges.insert(GITHUB.into(),json!({"hash":"host","observedAt":context["snapshot"]["fetchedAt"],"badges":badges}));
        }
    } else if matches!(
        context["status"].as_str(),
        Some("unavailable" | "workspace_unavailable")
    ) {
        if let Some(record) = data.script_badges.get_mut(GITHUB) {
            record["observedAt"] = json!(0);
        }
    } else {
        data.script_badges.remove(GITHUB);
    }
    if data.script_badges != previous {
        let old_revision = data.badge_revision;
        data.badge_revision = data.badge_revision.saturating_add(1);
        if s.store.lock().unwrap().save_state(&s.id, &data).is_err() {
            data.badge_revision = old_revision;
            data.script_badges = previous;
            return Err("Could not save GitHub badges".into());
        }
    }
    Ok(())
}
pub async fn context(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<crate::github::Refresh>,
) -> ApiResult {
    let s = session(&app, &id)?;
    let Json(mut value) =
        crate::github::context(State(app.clone()), Path(id), Query(query)).await?;
    sync_github(&s, &value).map_err(|e| fail(StatusCode::INSUFFICIENT_STORAGE, e))?;
    let data = s.data.lock().unwrap();
    let store = s.store.lock().unwrap();
    let result = payload(&data, &store.connection);
    value["badges"] = result["badges"].clone();
    value["badgesAt"] = result["badgesAt"].clone();
    value["badgeRevision"] = result["badgeRevision"].clone();
    Ok(Json(value))
}
/// Reads stored state only: never fetches GitHub or runs a script.
pub async fn list(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let data = s.data.lock().unwrap();
    let store = s.store.lock().unwrap();
    Ok(Json(payload(&data, &store.connection)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn badge_values_and_links_are_bounded() {
        let mut badge = json!({"id":"build","label":"Build","tone":"danger","tooltip":"Failed"});
        assert!(validate(&json!([badge.clone()]), "a.star").is_ok());
        for url in [
            "javascript:alert(1)",
            "http://example.com",
            "https://user:secret@example.com",
            "/relative",
        ] {
            badge["url"] = json!(url);
            assert!(validate(&json!([badge.clone()]), "a.star").is_err());
        }
        badge["url"] = json!("https://github.com/org/repo/pull/1");
        assert!(validate(&json!([badge]), "a.star").is_ok());
    }
}
