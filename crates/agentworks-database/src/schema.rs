pub(crate) const INBOX_DELIVERY: &str = "
CREATE TABLE IF NOT EXISTS inbox_policies(session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE, policy TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS inbox_policy_requests(session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, request_id TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(session_id,request_id));
CREATE TABLE IF NOT EXISTS inbox_deliveries(message_id INTEGER PRIMARY KEY REFERENCES agent_messages(id),recipient TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,generation INTEGER NOT NULL,state TEXT NOT NULL,request_id TEXT,reason TEXT);
CREATE INDEX IF NOT EXISTS inbox_delivery_pending ON inbox_deliveries(recipient,state,message_id);
CREATE TABLE IF NOT EXISTS reply_requests(message_id INTEGER PRIMARY KEY REFERENCES agent_messages(id),requester TEXT NOT NULL,reply_id INTEGER);
CREATE INDEX IF NOT EXISTS reply_requests_reply ON reply_requests(reply_id);
";

pub(crate) const CONTROLLERS: &str = "CREATE TABLE IF NOT EXISTS agent_controllers(id TEXT PRIMARY KEY,name TEXT NOT NULL,project_id TEXT NOT NULL,grants TEXT NOT NULL,token_hash TEXT NOT NULL UNIQUE,created INTEGER NOT NULL,revoked INTEGER,last_used INTEGER);
  CREATE INDEX IF NOT EXISTS agent_controllers_project ON agent_controllers(project_id,created);";

pub(crate) const ROLES: &str =
    "CREATE TABLE IF NOT EXISTS roles(id TEXT PRIMARY KEY,definition TEXT NOT NULL);";

/// Durable scheduler state. `automation_schedules.cursor` is the instant up to
/// which occurrences are handled; `automation_slots` records each handled
/// occurrence once. Started runs are keyed `schedule:<slot>` in
/// `automation_runs`, so a crash between starting and recording cannot start
/// the same occurrence twice. `automation_agent_requests` makes agent
/// mutations idempotent per `(actor, requestId)` (schema v12).

pub(crate) const AUTOMATIONS: &str = "
CREATE TABLE IF NOT EXISTS automation_schedules(automation_id TEXT PRIMARY KEY,cursor INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS automation_slots(automation_id TEXT NOT NULL,slot INTEGER NOT NULL,request_id TEXT NOT NULL,outcome TEXT NOT NULL,detail TEXT,created INTEGER NOT NULL,PRIMARY KEY(automation_id,slot));
CREATE TABLE IF NOT EXISTS automation_agent_requests(actor TEXT NOT NULL,request_id TEXT NOT NULL,operation TEXT NOT NULL,payload TEXT NOT NULL,result TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(actor,request_id));
CREATE TABLE IF NOT EXISTS automation_github_baselines(automation_id TEXT PRIMARY KEY,scope TEXT NOT NULL,snapshot TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS automation_events(id TEXT PRIMARY KEY,automation_id TEXT NOT NULL,revision INTEGER NOT NULL,payload TEXT NOT NULL,created INTEGER NOT NULL,status TEXT NOT NULL DEFAULT 'pending',action TEXT,target TEXT,detail TEXT);
CREATE TABLE IF NOT EXISTS automation_github_polls(automation_id TEXT PRIMARY KEY,checked_at INTEGER NOT NULL,error TEXT);
CREATE TABLE IF NOT EXISTS automation_event_actions(event_id TEXT NOT NULL,action_key TEXT NOT NULL,payload TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'pending',target TEXT,detail TEXT,PRIMARY KEY(event_id,action_key));
CREATE TABLE IF NOT EXISTS automation_trigger_baselines(automation_id TEXT NOT NULL,trigger_name TEXT NOT NULL,scope TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(automation_id,trigger_name));
INSERT OR IGNORE INTO automation_trigger_baselines SELECT automation_id,'default',scope,snapshot FROM automation_github_baselines;
DELETE FROM automation_github_baselines;
CREATE INDEX IF NOT EXISTS automation_events_pending ON automation_events(automation_id,status);
";

pub(crate) const GITHUB_TRACKING: &str = "
CREATE TABLE IF NOT EXISTS github_pr_tracking(session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,scope TEXT,snapshot TEXT NOT NULL DEFAULT '{}',status TEXT NOT NULL DEFAULT 'unknown',observed_at INTEGER,next_poll INTEGER NOT NULL DEFAULT 0,hooks TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS github_pr_events(id INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,payload TEXT NOT NULL,created INTEGER NOT NULL,delivered INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS github_pr_events_by_session ON github_pr_events(session_id,id);
";

pub(crate) const SCRIPTS: &str = "CREATE TABLE IF NOT EXISTS scripts(name TEXT PRIMARY KEY,capability TEXT NOT NULL,source TEXT NOT NULL,hash TEXT NOT NULL,enabled INTEGER NOT NULL,builtin INTEGER NOT NULL,updated INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS script_order(name TEXT PRIMARY KEY,priority INTEGER NOT NULL);";

/// The compaction cursor per session, and when storage maintenance (vacuum) last ran.
pub(crate) const EVENT_COMPACTION: &str = "CREATE TABLE IF NOT EXISTS event_compaction(session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,through INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS maintenance(name TEXT PRIMARY KEY,at INTEGER NOT NULL);";
