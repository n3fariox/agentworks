// Notification policy, browser popup stream, and durable workspace inbox.
//
// The inbox records noteworthy events independently of browser popup policy.
// Saved policy controls the separate optional popup stream; the browser owns
// permission and popup de-duplication across tabs.
//
// Streaming ACP updates are deliberately not notable events: a single turn emits
// them per token and per tool call. The categories below cover the events a
// person actually needs to be told about.
use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

const CHANNEL: usize = 256;
const MAX_BODY: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    TurnEnd,
    Approval,
    Error,
}

impl Event {
    pub const ALL: [Event; 3] = [Event::TurnEnd, Event::Approval, Event::Error];
    pub fn label(self) -> &'static str {
        match self {
            Event::TurnEnd => "Turn finished",
            Event::Approval => "Approval or question",
            Event::Error => "Turn failed",
        }
    }
}

/// Events that mean "a person may want to look at this session".
pub fn classify(kind: &str, data: &Value) -> Option<Event> {
    match kind {
        "approval" => Some(Event::Approval),
        "turn_end" if data.get("timedOut").is_some() => Some(Event::Error),
        "turn_end" => Some(Event::TurnEnd),
        "error" => Some(Event::Error),
        "interrupted" => Some(Event::Error),
        _ => None,
    }
}

fn truncate(text: &str) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= MAX_BODY {
        return cleaned;
    }
    let mut out: String = cleaned.chars().take(MAX_BODY - 1).collect();
    out.push('…');
    out
}

pub fn body(category: Event, data: &Value) -> String {
    let text = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match category {
        Event::TurnEnd => match data.get("timedOut").and_then(Value::as_str) {
            Some("total") => "Stopped by the total turn limit.".into(),
            Some(_) => "Stopped by the inactivity timeout.".into(),
            None => match data.get("stopReason").and_then(Value::as_str) {
                Some("cancelled") => "Turn cancelled.".into(),
                _ => "Turn finished.".into(),
            },
        },
        Event::Approval if text("kind") == "question" => "Waiting for your answer.".into(),
        Event::Approval => "Waiting for your approval.".into(),
        Event::Error if data.get("timedOut").is_some() => body(Event::TurnEnd, data),
        Event::Error => match text("message") {
            m if m.is_empty() => "Turn failed.".into(),
            m => truncate(&m),
        },
    }
}

/// Per-project overrides. Absent fields inherit the saved global setting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectRule {
    pub enabled: Option<bool>,
    pub events: Option<BTreeSet<Event>>,
}

/// Container-level `default` fills missing fields from this impl, so an absent or
/// partial catalog entry keeps "off, every category" rather than an empty set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub enabled: bool,
    pub events: BTreeSet<Event>,
    pub projects: BTreeMap<String, ProjectRule>,
    pub muted_sessions: BTreeSet<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            events: Event::ALL.into_iter().collect(),
            projects: BTreeMap::new(),
            muted_sessions: BTreeSet::new(),
        }
    }
}

impl Settings {
    /// The categories to notify for one session, or None when it is muted.
    pub fn effective(&self, session_id: &str, project_id: Option<&str>) -> Option<BTreeSet<Event>> {
        if self.muted_sessions.contains(session_id) {
            return None;
        }
        let rule = project_id.and_then(|id| self.projects.get(id));
        if !rule.and_then(|r| r.enabled).unwrap_or(self.enabled) {
            return None;
        }
        Some(
            rule.and_then(|r| r.events.clone())
                .unwrap_or_else(|| self.events.clone()),
        )
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.muted_sessions.len() > 5000 {
            return Err("Too many muted sessions".into());
        }
        if self.projects.len() > 500 {
            return Err("Too many project notification rules".into());
        }
        if self
            .projects
            .keys()
            .chain(self.muted_sessions.iter())
            .any(|id| id.is_empty() || id.len() > 64 || id.contains(char::is_control))
        {
            return Err("Project or session identifiers are not valid".into());
        }
        Ok(())
    }
}

/// Durable workspace inbox, independent of optional browser popup policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub question: bool,
    pub seq: u64,
    pub title: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub event: String,
    pub body: String,
    pub created: u64,
    pub approval_id: Option<String>,
    pub resolved: bool,
    pub dismissed: bool,
}

/// Everything the stream needs to address one session.
pub struct Context<'a> {
    pub session_id: &'a str,
    pub title: &'a str,
    pub provider: &'a str,
    pub project_id: Option<&'a str>,
}

/// Fans noteworthy events out to every connected browser.
#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<Value>,
    inbox_tx: broadcast::Sender<Value>,
    catalog: Arc<Mutex<Catalog>>,
}

impl Hub {
    pub fn new(catalog: Arc<Mutex<Catalog>>) -> Self {
        let (tx, _) = broadcast::channel(CHANNEL);
        let (inbox_tx, _) = broadcast::channel(CHANNEL);
        Hub {
            tx,
            inbox_tx,
            catalog,
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }
    pub fn subscribe_inbox(&self) -> broadcast::Receiver<Value> {
        self.inbox_tx.subscribe()
    }
    pub fn list(&self) -> Vec<Alert> {
        self.catalog
            .lock()
            .unwrap()
            .data
            .notification_inbox
            .iter()
            .filter(|a| !a.dismissed)
            .cloned()
            .rev()
            .collect()
    }
    pub fn dismiss(&self, ids: &[String], dismissed: bool) -> std::io::Result<()> {
        let mut catalog = self.catalog.lock().unwrap();
        let mut alerts = catalog.data.notification_inbox.clone();
        let ids: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
        for alert in &mut alerts {
            if ids.contains(alert.id.as_str()) {
                alert.dismissed = dismissed;
            }
        }
        catalog.notification_inbox(alerts)?;
        let _ = self.inbox_tx.send(json!({"changed":true}));
        Ok(())
    }
    pub fn automation_issue(
        &self,
        definition: &crate::automations::Definition,
        occurrence: &str,
        time: u64,
        detail: &str,
    ) {
        let result = (|| -> std::io::Result<()> {
            let mut catalog = self.catalog.lock().unwrap();
            let mut alerts = catalog.data.notification_inbox.clone();
            let id = format!("automation:{}:{}", definition.id, occurrence);
            if alerts.iter().any(|a| a.id == id) {
                return Ok(());
            }
            alerts.push(Alert {
                id,
                session_id: String::new(),
                seq: 0,
                automation_id: Some(definition.id.clone()),
                question: false,
                title: definition.name.clone(),
                project_id: definition.project_id.clone(),
                project_name: definition
                    .project_id
                    .as_deref()
                    .and_then(|id| catalog.data.projects.iter().find(|p| p.id == id))
                    .map(|p| p.name.clone()),
                event: "automationIssue".into(),
                body: truncate(detail),
                created: time,
                approval_id: None,
                resolved: false,
                dismissed: false,
            });
            catalog.notification_inbox(alerts)?;
            let _ = self.inbox_tx.send(json!({"changed":true}));
            Ok(())
        })();
        if let Err(error) = result {
            tracing::error!(%error, "Could not persist automation alert");
        }
    }
    pub fn observe(&self, context: &Context, seq: u64, time: u64, kind: &str, data: &Value) {
        let category = classify(kind, data);
        if category.is_none() && !matches!(kind, "completion_reported" | "approval_resolved") {
            return;
        }
        let result = (|| -> std::io::Result<()> {
            let mut catalog = self.catalog.lock().unwrap();
            let mut alerts = catalog.data.notification_inbox.clone();
            if kind == "approval_resolved" {
                let approval_id = data["id"].as_str();
                let mut changed = false;
                for alert in &mut alerts {
                    if alert.session_id == context.session_id
                        && alert.event == "approval"
                        && approval_id.is_some()
                        && alert.approval_id.as_deref() == approval_id
                        && !alert.resolved
                    {
                        alert.resolved = true;
                        alert.body = "This request has been resolved.".into();
                        changed = true;
                    }
                }
                if !changed {
                    return Ok(());
                }
            } else {
                let id = format!("{}:{}", context.session_id, seq);
                if alerts.iter().any(|a| a.id == id) {
                    return Ok(());
                }
                let (event, body) = match category {
                    Some(category) => (
                        serde_json::to_value(category)
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_string(),
                        body(category, data),
                    ),
                    None => (
                        if data["status"] == "blocked" {
                            "taskBlocked"
                        } else {
                            "taskCompleted"
                        }
                        .into(),
                        truncate(
                            data["summary"]
                                .as_str()
                                .unwrap_or("An explicit completion report is available."),
                        ),
                    ),
                };
                alerts.push(Alert {
                    id,
                    session_id: context.session_id.into(),
                    automation_id: None,
                    question: kind == "approval" && data["kind"] == "question",
                    seq,
                    title: context.title.into(),
                    project_id: context.project_id.map(str::to_string),
                    project_name: context
                        .project_id
                        .and_then(|id| catalog.data.projects.iter().find(|p| p.id == id))
                        .map(|p| p.name.clone()),
                    event,
                    body,
                    created: time,
                    approval_id: if kind == "approval" {
                        data["id"].as_str().map(str::to_string)
                    } else {
                        None
                    },
                    resolved: false,
                    dismissed: false,
                });
            }
            // Undismissed alerts never expire. Keep recent dismissed records for Undo.
            let mut dismissed = alerts.iter().filter(|a| a.dismissed).count();
            alerts.retain(|a| {
                if a.dismissed && dismissed > 1000 {
                    dismissed -= 1;
                    false
                } else {
                    true
                }
            });
            catalog.notification_inbox(alerts)?;
            let _ = self.inbox_tx.send(json!({"changed":true}));
            Ok(())
        })();
        if let Err(error) = result {
            tracing::error!(%error, "Could not persist notification inbox");
        }
        if let Some(category) = category {
            self.publish(context, seq, category, data);
        }
    }
    /// `seq` makes the identifier stable per session event so a reconnecting or
    /// duplicated tab can recognise a notification it already showed.
    pub fn publish(&self, context: &Context, seq: u64, category: Event, data: &Value) {
        let allowed = {
            let catalog = self.catalog.lock().unwrap();
            catalog
                .data
                .settings
                .notifications
                .effective(context.session_id, context.project_id)
        };
        let Some(events) = allowed else { return };
        if !events.contains(&category) {
            return;
        }
        let _ = self.tx.send(json!({
            "id":format!("{}:{}", context.session_id, seq),
            "sessionId":context.session_id,
            "title":context.title,
            "provider":context.provider,
            "projectId":context.project_id,
            "event":category,
            "body":body(category, data),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::default()
    }

    #[test]
    fn streaming_updates_and_heartbeats_are_not_notable() {
        assert_eq!(
            classify("acp", &json!({"sessionUpdate":"agent_message_chunk"})),
            None
        );
        assert_eq!(classify("input_queue", &json!({})), None);
        assert_eq!(classify("timeout_policy", &json!({})), None);
        assert_eq!(classify("status", &json!({"status":"idle"})), None);
        // An approval emits `status:waiting` too; only the approval itself notifies.
        assert_eq!(classify("status", &json!({"status":"waiting"})), None);
        assert_eq!(classify("status", &json!({"status":"running"})), None);
    }

    #[test]
    fn notable_kinds_map_to_categories() {
        assert_eq!(classify("approval", &json!({})), Some(Event::Approval));
        assert_eq!(classify("turn_end", &json!({})), Some(Event::TurnEnd));
        assert_eq!(
            classify("turn_end", &json!({"timedOut":"total"})),
            Some(Event::Error)
        );
        assert_eq!(classify("error", &json!({})), Some(Event::Error));
        assert_eq!(classify("interrupted", &json!({})), Some(Event::Error));
        assert_eq!(classify("completion_reported", &json!({})), None);
    }

    #[test]
    fn disabled_by_default_and_muting_wins() {
        let s = settings();
        assert_eq!(s.effective("a", None), None);
        let s = Settings {
            enabled: true,
            ..settings()
        };
        assert_eq!(s.effective("a", None).unwrap().len(), Event::ALL.len());
        let s = Settings {
            muted_sessions: ["a".to_string()].into(),
            ..s
        };
        assert_eq!(s.effective("a", None), None);
        assert!(s.effective("b", None).is_some());
    }

    #[test]
    fn project_rule_overrides_the_global_setting() {
        let s = Settings {
            enabled: true,
            projects: [(
                "p1".to_string(),
                ProjectRule {
                    enabled: Some(false),
                    events: None,
                },
            )]
            .into(),
            ..settings()
        };
        assert_eq!(s.effective("a", Some("p1")), None);
        assert!(s.effective("a", Some("p2")).is_some());
        assert!(s.effective("a", None).is_some());
    }

    #[test]
    fn project_categories_narrow_without_disabling() {
        let s = Settings {
            enabled: true,
            projects: [(
                "p1".to_string(),
                ProjectRule {
                    enabled: None,
                    events: Some([Event::Approval].into()),
                },
            )]
            .into(),
            ..settings()
        };
        let events = s.effective("a", Some("p1")).unwrap();
        assert!(events.contains(&Event::Approval));
        assert!(!events.contains(&Event::TurnEnd));
    }

    #[test]
    fn a_project_can_re_enable_while_the_global_setting_is_off() {
        let s = Settings {
            projects: [(
                "p1".to_string(),
                ProjectRule {
                    enabled: Some(true),
                    events: None,
                },
            )]
            .into(),
            ..settings()
        };
        assert!(s.effective("a", Some("p1")).is_some());
        assert_eq!(s.effective("a", Some("p2")), None);
    }

    #[test]
    fn partial_catalog_entries_keep_the_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.enabled);
        assert_eq!(s.events.len(), Event::ALL.len());
        let s: Settings = serde_json::from_str("{\"enabled\":true}").unwrap();
        assert_eq!(s.events.len(), Event::ALL.len());
    }

    #[test]
    fn bodies_name_the_outcome_without_inventing_detail() {
        assert_eq!(body(Event::TurnEnd, &json!({})), "Turn finished.");
        assert_eq!(
            body(Event::TurnEnd, &json!({"stopReason":"cancelled"})),
            "Turn cancelled."
        );
        assert_eq!(
            body(Event::TurnEnd, &json!({"timedOut":"total"})),
            "Stopped by the total turn limit."
        );
        assert_eq!(
            body(Event::TurnEnd, &json!({"timedOut":"inactivity"})),
            "Stopped by the inactivity timeout."
        );
        assert_eq!(body(Event::Error, &json!({})), "Turn failed.");
        let long = "x".repeat(MAX_BODY * 2);
        assert_eq!(
            body(Event::Error, &json!({"message":long})).chars().count(),
            MAX_BODY
        );
    }

    #[test]
    fn hub_filters_policy_and_preserves_event_identity() {
        let root = std::env::temp_dir().join(format!("u02-hub-{}", uuid::Uuid::new_v4()));
        let catalog = Arc::new(Mutex::new(
            Catalog::open(root.join("catalog.json")).unwrap(),
        ));
        let hub = Hub::new(catalog.clone());
        let mut rx = hub.subscribe();
        let context = Context {
            session_id: "session",
            title: "Source",
            provider: "demo",
            project_id: Some("project"),
        };
        hub.publish(&context, 10, Event::Approval, &json!({}));
        assert!(rx.try_recv().is_err());
        catalog
            .lock()
            .unwrap()
            .notifications(Settings {
                enabled: true,
                events: [Event::Approval].into(),
                ..Settings::default()
            })
            .unwrap();
        hub.publish(&context, 11, Event::TurnEnd, &json!({}));
        assert!(rx.try_recv().is_err());
        hub.publish(
            &context,
            12,
            Event::Approval,
            &json!({"request":{"toolCall":{"kind":"question"}}}),
        );
        let notice = rx.try_recv().unwrap();
        assert_eq!(notice["id"], "session:12");
        assert_eq!(notice["sessionId"], "session");
        assert_eq!(notice["title"], "Source");
        assert_eq!(notice["event"], "approval");
        assert!(rx.try_recv().is_err());
        // Reconnect gets no historical backlog.
        assert!(hub.subscribe().try_recv().is_err());
        let reopened = Catalog::open(root.join("catalog.json")).unwrap();
        assert!(reopened.data.settings.notifications.enabled);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inbox_survives_restart_dismissal_and_resolution_without_popup_permission() {
        let root =
            std::env::temp_dir().join(format!("notification-inbox-{}", uuid::Uuid::new_v4()));
        let path = root.join("catalog.json");
        let catalog = Arc::new(Mutex::new(Catalog::open(path.clone()).unwrap()));
        let hub = Hub::new(catalog);
        let context = Context {
            session_id: "source",
            title: "Work",
            provider: "demo",
            project_id: None,
        };
        let mut changes = hub.subscribe_inbox();
        let mut popups = hub.subscribe();
        hub.observe(
            &context,
            1,
            100,
            "approval",
            &json!({"id":"approval-1","kind":"question"}),
        );
        hub.observe(&context, 2, 200, "turn_end", &json!({}));
        hub.observe(&context, 2, 200, "turn_end", &json!({}));
        assert_eq!(hub.list().len(), 2);
        assert!(hub.list()[1].question);
        assert!(changes.try_recv().is_ok());
        assert!(popups.try_recv().is_err());
        hub.dismiss(&["source:1".into()], true).unwrap();
        hub.observe(
            &context,
            3,
            300,
            "approval_resolved",
            &json!({"id":"approval-1"}),
        );
        assert_eq!(hub.list().len(), 1); // Resolving never resurrects a dismissed alert.
        let reopened = Hub::new(Arc::new(Mutex::new(Catalog::open(path).unwrap())));
        assert_eq!(reopened.list()[0].id, "source:2");
        reopened.dismiss(&["source:1".into()], false).unwrap();
        assert!(reopened.list()[1].resolved);
        reopened.observe(
            &context,
            4,
            400,
            "completion_reported",
            &json!({"status":"completed","summary":"Done"}),
        );
        reopened.observe(
            &context,
            5,
            500,
            "completion_reported",
            &json!({"status":"blocked","summary":"Need credentials"}),
        );
        assert_eq!(reopened.list()[0].event, "taskBlocked");
        assert_eq!(reopened.list()[1].event, "taskCompleted");
        assert_eq!(reopened.list()[1].body, "Done");
        assert_eq!(reopened.list()[2].event, "turnEnd");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validation_rejects_unbounded_control_data() {
        let mut s = Settings::default();
        s.projects
            .insert("bad\nid".to_string(), ProjectRule::default());
        assert!(s.validate().is_err());
        assert!(settings().validate().is_ok());
    }
}
