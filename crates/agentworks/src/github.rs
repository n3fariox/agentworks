//! GitHub context. Git metadata may come from workers; credentials and API
//! traffic always belong to this server's OS user. No agent write tools here.
use crate::{ApiResult, App, fail, session};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt, process::Command, sync::Mutex};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub connections: Vec<Connection>,
    /// Repository key includes the GitHub host; worktrees share this selection.
    #[serde(default)]
    pub bindings: HashMap<String, String>,
    #[serde(default)]
    pub links: HashMap<String, Vec<Link>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connection {
    pub host: String,
    pub login: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Link {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub kind: String,
    pub number: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Repository {
    pub host: String,
    pub owner: String,
    pub name: String,
}
impl Repository {
    fn key(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.name).to_ascii_lowercase()
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    host: String,
    login: String,
    state: String,
}
#[derive(Default)]
pub struct Hub {
    state: Mutex<HubState>,
}
#[derive(Default)]
struct HubState {
    discovery: Option<(Instant, Result<Vec<Account>, String>)>,
    snapshots: HashMap<String, (Instant, Value)>,
    failures: HashMap<String, (u64, String)>,
    // Serialize main-host requests and share work between sessions. Capped cache;
    // neither credentials nor Octocrab clients are persisted in this state.
}
fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|part| {
            !part.is_empty()
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
/// Literal HTTPS/SSH hostnames only. Do not interpret SSH aliases or URL rewrites.
pub fn parse_origin(origin: &str) -> Option<Repository> {
    let (host, path) = if let Some(rest) = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("ssh://"))
    {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()?, path)
    } else {
        let (authority, path) = origin.split_once(':')?;
        (authority.rsplit('@').next()?, path)
    };
    let host = host.to_ascii_lowercase();
    if !valid_host(&host) || path.contains(['?', '#']) {
        return None;
    }
    let path = path
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_end_matches('/'));
    let (owner, name) = path.split_once('/')?;
    if !valid_name(owner) || !valid_name(name) {
        return None;
    }
    Some(Repository {
        host,
        owner: owner.into(),
        name: name.into(),
    })
}
fn is_enterprise_server(host: &str) -> bool {
    host != "github.com" && !host.ends_with(".ghe.com")
}
fn api_base(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com/".into()
    } else if host.ends_with(".ghe.com") {
        format!("https://api.{host}/")
    } else {
        format!("https://{host}/api/v3/")
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Capture bounded stdout privately. Never put gh output/errors in diagnostics:
/// status JSON and even stderr can contain credentials or private configuration.
async fn gh(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("gh");
    for key in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GH_HOST",
        "GH_REPO",
        "GH_DEBUG",
        "DEBUG",
    ] {
        command.env_remove(key);
    }
    command
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| {
        "GitHub CLI is unavailable on the main host. Install gh and sign in there.".to_string()
    })?;
    let stdout = child.stdout.take().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let read = async {
            let mut bytes = Vec::new();
            stdout.take(65537).read_to_end(&mut bytes).await.map_err(|_| "Could not read GitHub CLI output")?;
            if bytes.len() > 65536 { return Err("GitHub CLI output exceeded its limit"); }
            String::from_utf8(bytes).map_err(|_| "Invalid GitHub CLI output")
        };
        let (text, status) = tokio::try_join!(read, async { child.wait().await.map_err(|_| "GitHub CLI failed") })?;
        if !status.success() { return Err("GitHub CLI could not retrieve the selected account. Check gh auth status on the main host."); }
        Ok(text)
    }).await;
    match result {
        Ok(result) => result.map_err(str::to_owned),
        Err(_) => Err("GitHub CLI timed out on the main host".into()),
    }
}
fn accounts(value: &Value) -> Vec<Account> {
    let mut result = Vec::new();
    if let Some(hosts) = value["hosts"].as_object() {
        for (host, entries) in hosts {
            if !valid_host(host) {
                continue;
            }
            for entry in entries.as_array().into_iter().flatten() {
                let Some(login) = entry["login"].as_str().filter(|s| valid_name(s)) else {
                    continue;
                };
                if result.iter().any(|a: &Account| {
                    a.host == host.to_ascii_lowercase() && a.login.eq_ignore_ascii_case(login)
                }) {
                    continue;
                }
                result.push(Account {
                    host: host.to_ascii_lowercase(),
                    login: login.into(),
                    state: if entry["state"] == "success" {
                        "available"
                    } else {
                        "unavailable"
                    }
                    .into(),
                });
            }
        }
    }
    result.sort_by(|a, b| (&a.host, &a.login).cmp(&(&b.host, &b.login)));
    result
}
async fn discover(hub: &mut HubState, refresh: bool) -> Result<Vec<Account>, String> {
    if let Some((at, result)) = &hub.discovery {
        if at.elapsed() < Duration::from_secs(if refresh { 5 } else { 60 }) {
            return result.clone();
        }
    }
    let result = match gh(&["auth", "status", "--json", "hosts"]).await {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map(|v| accounts(&v))
            .map_err(|_| {
                "GitHub CLI returned an unsupported response. Update gh on the main host.".into()
            }),
        Err(error) => Err(error),
    };
    hub.discovery = Some((Instant::now(), result.clone()));
    result
}
#[derive(Default, Deserialize)]
pub struct Refresh {
    #[serde(default)]
    refresh: bool,
}
pub async fn connections(State(app): State<App>, Query(query): Query<Refresh>) -> ApiResult {
    let mut hub = app.github.state.lock().await;
    let result = discover(&mut hub, query.refresh).await;
    let config = app.catalog.lock().unwrap().data.github.clone();
    let mut rows = Vec::new();
    for account in result.as_ref().into_iter().flatten() {
        let enabled = config.connections.iter().any(|c| {
            c.host == account.host && c.login.eq_ignore_ascii_case(&account.login) && c.enabled
        });
        rows.push(json!({"host":account.host,"login":account.login,"state":account.state,"enabled":enabled,"apiBase":api_base(&account.host)}));
    }
    for saved in &config.connections {
        if !rows.iter().any(|r| {
            r["host"] == saved.host
                && r["login"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&saved.login))
        }) {
            rows.push(json!({"host":saved.host,"login":saved.login,"state":"unavailable","enabled":saved.enabled,"apiBase":api_base(&saved.host)}));
        }
    }
    Ok(Json(
        json!({"connections":rows,"message":result.err(),"credentialHost":"local"}),
    ))
}
pub async fn save_connection(
    State(app): State<App>,
    Json(mut input): Json<Connection>,
) -> ApiResult {
    input.host = input.host.to_ascii_lowercase();
    if !valid_host(&input.host) || !valid_name(&input.login) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Choose a valid GitHub host and account",
        ));
    }
    let mut hub = app.github.state.lock().await;
    if input.enabled {
        let rows = discover(&mut hub, true)
            .await
            .map_err(|e| fail(StatusCode::CONFLICT, e))?;
        let found = rows
            .iter()
            .any(|r| r.host == input.host && r.login.eq_ignore_ascii_case(&input.login));
        if !found {
            return Err(fail(
                StatusCode::CONFLICT,
                "This account is not signed in on the main host",
            ));
        }
    }
    {
        let mut catalog = app.catalog.lock().unwrap();
        let mut config = catalog.data.github.clone();
        config
            .connections
            .retain(|c| !(c.host == input.host && c.login.eq_ignore_ascii_case(&input.login)));
        if config.connections.len() >= 40 {
            return Err(fail(
                StatusCode::CONFLICT,
                "At most 40 GitHub connections are supported",
            ));
        }
        config.connections.push(input);
        catalog.github(config).map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not save GitHub connection",
            )
        })?;
    }
    hub.snapshots.clear();
    hub.failures.clear();
    Ok(Json(json!({"ok":true})))
}
/// Legacy single-account gh configurations may have only a host-level token.
/// Named-user lookup can then enter the keyring even though active lookup works.
/// Select the active path only from fresh, successful identity metadata, and
/// always verify the returned token independently before repository access.
fn active_account_matches(status: &Value, connection: &Connection) -> bool {
    let active: Vec<_> = status["hosts"][&connection.host]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["active"] == true)
        .collect();
    matches!(active.as_slice(), [entry] if entry["state"] == "success"
        && entry["login"].as_str().is_some_and(|login| login.eq_ignore_ascii_case(&connection.login)))
}
async fn client(connection: &Connection) -> Result<octocrab::Octocrab, Failure> {
    let status = gh(&[
        "auth",
        "status",
        "--hostname",
        &connection.host,
        "--active",
        "--json",
        "hosts",
    ])
    .await?;
    let status: Value = serde_json::from_str(&status)
        .map_err(|_| "GitHub CLI returned invalid account metadata")?;
    let mut args = vec!["auth", "token", "--hostname", connection.host.as_str()];
    if !active_account_matches(&status, connection) {
        args.extend(["--user", connection.login.as_str()]);
    }
    let token = gh(&args).await?;
    if token.trim().is_empty() {
        return Err("GitHub CLI returned no credential".into());
    }
    let transport = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not configure GitHub transport")?;
    let client = octocrab::Octocrab::builder()
        .personal_token(token.trim().to_owned())
        .base_uri(api_base(&connection.host).trim_end_matches("api/v3/"))
        .map_err(|_| "Invalid GitHub API address")?
        .set_connect_timeout(Some(Duration::from_secs(5)))
        .set_read_timeout(Some(Duration::from_secs(15)))
        .build_with_reqwest(transport)
        .map_err(|_| "Could not configure GitHub client")?;
    // Validate identity on every new credential resolution, including after a gh
    // account switch or environment change. Tokens never leave this function.
    verify_identity(&client, connection).await?;
    Ok(client)
}
async fn verify_identity(
    client: &octocrab::Octocrab,
    connection: &Connection,
) -> Result<(), Failure> {
    let user: Value = client
        .get(
            if is_enterprise_server(&connection.host) {
                "/api/v3/user"
            } else {
                "/user"
            },
            None::<&()>,
        )
        .await
        .map_err(|e| api_error(&e))?;
    if !user["login"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case(&connection.login))
    {
        return Err("GitHub credential identity did not match the selected account".into());
    }
    Ok(())
}
#[derive(Clone)]
struct Failure {
    message: String,
    retry_at: u64,
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            retry_at: now() + 30_000,
        }
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
fn api_error(error: &octocrab::Error) -> Failure {
    if let octocrab::Error::GitHub { source, .. } = error {
        let message = match source.status_code.as_u16() {
            401 => "GitHub authentication failed; sign in again on the main host",
            403 | 429 => {
                "GitHub access is denied or rate limited; check permissions and retry later"
            }
            404 => "GitHub resource is unavailable or this account cannot access it",
            _ => "GitHub API request failed; retry later",
        };
        let mut failure = Failure::from(message);
        if matches!(source.status_code.as_u16(), 403 | 429) {
            let delay = source
                .headers
                .as_ref()
                .and_then(|h| h.get("retry-after"))
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60);
            failure.retry_at = now().saturating_add(delay.saturating_mul(1000));
            // A reset header is also present on ordinary permission failures.
            // Only primary-limit exhaustion justifies waiting for that window.
            let exhausted = source
                .headers
                .as_ref()
                .and_then(|h| h.get("x-ratelimit-remaining"))
                .is_some_and(|value| value == "0");
            if exhausted {
                failure.retry_at = failure
                    .retry_at
                    .max(source.rate_limit_reset.unwrap_or(0).saturating_mul(1000));
            }
        }
        return failure;
    }
    "GitHub could not be reached; check the main host's network and certificate configuration"
        .into()
}
pub async fn verify(State(app): State<App>, Json(input): Json<Connection>) -> ApiResult {
    let _hub = app.github.state.lock().await;
    let config = app.catalog.lock().unwrap().data.github.clone();
    let connection = config
        .connections
        .iter()
        .find(|c| c.enabled && c.host == input.host && c.login == input.login)
        .ok_or_else(|| fail(StatusCode::CONFLICT, "Enable this connection first"))?;
    let result = client(connection).await;
    Ok(Json(
        json!({"ok":result.is_ok(),"message":result.err().map(|e|e.message),"host":connection.host,"login":connection.login}),
    ))
}

async fn git(app: &App, id: &str) -> Result<Value, (StatusCode, String)> {
    crate::git_context::workspace(State(app.clone()), Path(id.to_owned()))
        .await
        .map(|v| v.0)
}
fn choose(config: &Config, repo: &Repository) -> Result<Connection, &'static str> {
    let matches: Vec<_> = config
        .connections
        .iter()
        .filter(|c| c.enabled && c.host == repo.host)
        .collect();
    if let Some(login) = config.bindings.get(&repo.key()) {
        return matches
            .into_iter()
            .find(|c| c.login.eq_ignore_ascii_case(login))
            .cloned()
            .ok_or("connection_unavailable");
    }
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err("unconfigured"),
        _ => Err("choose_account"),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingRequest {
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    links: Option<Vec<Link>>,
}
pub async fn save_binding(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<BindingRequest>,
) -> ApiResult {
    let _ = session(&app, &id)?;
    let context = git(&app, &id).await?;
    let repo = context["origin"]
        .as_str()
        .and_then(parse_origin)
        .ok_or_else(|| {
            fail(
                StatusCode::CONFLICT,
                "No supported GitHub origin in this workspace",
            )
        })?;
    let mut hub = app.github.state.lock().await;
    let mut catalog = app.catalog.lock().unwrap();
    let mut config = catalog.data.github.clone();
    if let Some(login) = input.login {
        if !config
            .connections
            .iter()
            .any(|c| c.enabled && c.host == repo.host && c.login.eq_ignore_ascii_case(&login))
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Select an enabled account for the origin host",
            ));
        }
        config.bindings.insert(repo.key(), login);
    }
    if let Some(links) = input.links {
        if links.len() > 10
            || links.iter().any(|l| {
                l.number == 0
                    || l.number > i32::MAX as u64
                    || !matches!(l.kind.as_str(), "issue" | "pullRequest")
                    || l.host != repo.host
                    || !l.owner.eq_ignore_ascii_case(&repo.owner)
                    || !l.repo.eq_ignore_ascii_case(&repo.name)
            })
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Link up to 10 issues or pull requests from this origin repository",
            ));
        }
        config.links.insert(id, links);
    }
    catalog.github(config).map_err(|_| {
        fail(
            StatusCode::INSUFFICIENT_STORAGE,
            "Could not save GitHub binding",
        )
    })?;
    hub.snapshots.clear();
    hub.failures.clear();
    Ok(Json(json!({"ok":true})))
}

pub async fn context(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<Refresh>,
) -> ApiResult {
    let context = match git(&app, &id).await {
        Ok(context) => context,
        Err((status, message))
            if status != StatusCode::NOT_FOUND && status != StatusCode::FORBIDDEN =>
        {
            return Ok(Json(
                json!({"status":"workspace_unavailable","message":message}),
            ));
        }
        Err(error) => return Err(error),
    };
    let Some(repo) = context["origin"].as_str().and_then(parse_origin) else {
        return Ok(Json(
            json!({"status":"no_origin","message":"This workspace has no supported HTTPS or SSH origin hostname."}),
        ));
    };
    let mut hub = app.github.state.lock().await;
    let config = app.catalog.lock().unwrap().data.github.clone();
    let mut view = json!({"repository":repo,"branch":context["branch"],"commit":context["commit"],"links":config.links.get(&id).cloned().unwrap_or_default()});
    let connection = match choose(&config, &repo) {
        Ok(connection) => connection,
        Err(state) => {
            view["status"] = json!(state);
            view["accounts"] = json!(
                config
                    .connections
                    .iter()
                    .filter(|c| c.enabled && c.host == repo.host)
                    .collect::<Vec<_>>()
            );
            view["message"] = json!(match state {
                "choose_account" => "Choose an account for this origin repository.",
                "connection_unavailable" =>
                    "The saved account is disabled or unavailable. Select another account or reconnect it in Settings.",
                _ =>
                    "Enable a main-host GitHub connection matching this origin hostname in Settings.",
            });
            return Ok(Json(view));
        }
    };
    // Pin an unambiguous choice durably so enabling another account later or
    // switching gh's active account cannot silently change this repository.
    if !config.bindings.contains_key(&repo.key()) {
        let mut catalog = app.catalog.lock().unwrap();
        let mut latest = catalog.data.github.clone();
        latest.bindings.insert(repo.key(), connection.login.clone());
        catalog.github(latest).map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not save GitHub binding",
            )
        })?;
    }
    view["connection"] = json!({"host":connection.host,"login":connection.login});
    view["accounts"] = json!(
        config
            .connections
            .iter()
            .filter(|c| c.enabled && c.host == repo.host)
            .collect::<Vec<_>>()
    );
    let links: Vec<Link> = config
        .links
        .get(&id)
        .into_iter()
        .flatten()
        .filter(|l| {
            l.host == repo.host
                && l.owner.eq_ignore_ascii_case(&repo.owner)
                && l.repo.eq_ignore_ascii_case(&repo.name)
        })
        .cloned()
        .collect();
    view["links"] = json!(links);
    let key = serde_json::to_string(&json!([
        repo.key(),
        connection.login.to_ascii_lowercase(),
        context["branch"],
        context["commit"],
        links
    ]))
    .unwrap();
    if let Some((at, cached)) = hub.snapshots.get(&key) {
        if at.elapsed() < Duration::from_secs(if query.refresh { 10 } else { 60 }) {
            view["snapshot"] = cached.clone();
            view["status"] = json!("ready");
            return Ok(Json(view));
        }
    }
    let connection_key = format!(
        "{}:{}",
        connection.host,
        connection.login.to_ascii_lowercase()
    );
    if let Some((until, message)) = hub.failures.get(&connection_key) {
        if *until > now() {
            view["status"] = json!("unavailable");
            view["message"] = json!(message);
            view["retryAt"] = json!(until);
            return Ok(Json(view));
        }
    }
    let fetched = tokio::time::timeout(Duration::from_secs(50), async {
        let client = client(&connection).await?;
        fetch_snapshot(
            &client,
            &repo,
            context["branch"].as_str(),
            context["commit"].as_str(),
            &links,
        )
        .await
    })
    .await
    .unwrap_or_else(|_| Err("GitHub refresh timed out; retry later".into()));
    match fetched {
        Ok(snapshot) => {
            if hub.snapshots.len() >= 128 {
                hub.snapshots.clear();
            }
            hub.snapshots
                .insert(key, (Instant::now(), snapshot.clone()));
            view["status"] = json!("ready");
            view["snapshot"] = snapshot;
        }
        Err(failure) => {
            if hub.failures.len() >= 128 {
                hub.failures.clear();
            }
            hub.failures
                .insert(connection_key, (failure.retry_at, failure.message.clone()));
            view["status"] = json!("unavailable");
            view["message"] = json!(failure.message);
            view["retryAt"] = json!(failure.retry_at);
        }
    }
    Ok(Json(view))
}
const PR_FIELDS: &str = r#"
 id number title url state isDraft headRefName headRefOid headRepository { nameWithOwner }
 baseRefName reviewDecision
 latestOpinionatedReviews(first:50) { pageInfo { hasNextPage } nodes { state commit { oid } } }
 commits(last:1) { nodes { commit { oid statusCheckRollup { state contexts(first:50) {
 pageInfo { hasNextPage } nodes {
 ... on CheckRun { name status conclusion detailsUrl }
 ... on StatusContext { context state targetUrl }
 } } } } } }
"#;
async fn graphql(
    client: &octocrab::Octocrab,
    host: &str,
    query: String,
    variables: Value,
) -> Result<Value, Failure> {
    let response: Value = client
        .post(
            if is_enterprise_server(host) {
                "/api/graphql"
            } else {
                "/graphql"
            },
            Some(&json!({"query":query,"variables":variables})),
        )
        .await
        .map_err(|e| api_error(&e))?;
    if response["errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty())
        || response["data"].is_null()
    {
        return Err("GitHub could not return complete repository context. Check access and GraphQL support on this host.".into());
    }
    Ok(response["data"].clone())
}
async fn fetch_snapshot(
    client: &octocrab::Octocrab,
    repo: &Repository,
    branch: Option<&str>,
    sha: Option<&str>,
    links: &[Link],
) -> Result<Value, Failure> {
    let query = format!(
        r#"query($owner:String!,$name:String!,$branch:String!) {{ repository(owner:$owner,name:$name) {{ id nameWithOwner url pullRequests(first:20,headRefName:$branch,orderBy:{{field:UPDATED_AT,direction:DESC}}) {{ pageInfo {{ hasNextPage }} nodes {{ {PR_FIELDS} }} }} }} }}"#
    );
    let mut resources = Vec::new();
    let mut complete = true;
    let mut repository = Value::Null;
    if let Some(branch) = branch {
        let data = graphql(
            client,
            &repo.host,
            query,
            json!({"owner":repo.owner,"name":repo.name,"branch":branch}),
        )
        .await?;
        if data["repository"].is_null() {
            return Err("Repository is unavailable to this account".into());
        }
        repository = json!({"id":data["repository"]["id"],"name":data["repository"]["nameWithOwner"],"url":data["repository"]["url"]});
        let prs = &data["repository"]["pullRequests"];
        complete = prs["pageInfo"]["hasNextPage"] == false;
        for pr in prs["nodes"].as_array().into_iter().flatten() {
            // A base repository may have PRs from fork branches of the same name.
            // Those cannot be inferred as this checkout's branch.
            let expected = format!("{}/{}", repo.owner, repo.name);
            if pr["headRepository"]["nameWithOwner"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(&expected))
            {
                resources.push(normalize(pr, "pullRequest", false, sha));
            }
        }
    }
    if branch.is_none() {
        let data = graphql(client, &repo.host,
            "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){id nameWithOwner url}}".into(),
            json!({"owner":repo.owner,"name":repo.name})).await?;
        if data["repository"].is_null() {
            return Err("Repository is unavailable to this account".into());
        }
        repository = json!({"id":data["repository"]["id"],"name":data["repository"]["nameWithOwner"],"url":data["repository"]["url"]});
    }
    for link in links {
        let query = format!(
            r#"query($owner:String!,$name:String!,$number:Int!) {{ repository(owner:$owner,name:$name) {{ id nameWithOwner url issueOrPullRequest(number:$number) {{ ... on Issue {{ id number title url state }} ... on PullRequest {{ {PR_FIELDS} }} }} }} }}"#
        );
        let data = graphql(
            client,
            &repo.host,
            query,
            json!({"owner":repo.owner,"name":repo.name,"number":link.number}),
        )
        .await?;
        let resource = &data["repository"]["issueOrPullRequest"];
        if resource.is_null() {
            return Err("A linked GitHub resource is unavailable to this account".into());
        }
        let kind = if resource.get("headRefOid").is_some() {
            "pullRequest"
        } else {
            "issue"
        };
        if kind != link.kind {
            return Err(
                "A linked resource has a different type; check its issue or PR number".into(),
            );
        }
        repository = json!({"id":data["repository"]["id"],"name":data["repository"]["nameWithOwner"],"url":data["repository"]["url"]});
        resources.retain(|r| r["number"] != resource["number"]);
        resources.push(normalize(resource, kind, true, sha));
    }
    Ok(
        json!({"fetchedAt":now(),"repository":repository,"resources":resources,"complete":complete,"headSha":sha}),
    )
}
fn review_summary(resource: &Value) -> Value {
    if let Some(decision) = resource["reviewDecision"]
        .as_str()
        .filter(|state| matches!(*state, "APPROVED" | "CHANGES_REQUESTED" | "REVIEW_REQUIRED"))
    {
        return json!({"state":decision,"complete":true,"source":"reviewDecision"});
    }
    let reviews = &resource["latestOpinionatedReviews"];
    if reviews["pageInfo"]["hasNextPage"] != false || !reviews["nodes"].is_array() {
        return json!({"state":"UNKNOWN","complete":false,"source":"reviews"});
    }
    let nodes = reviews["nodes"].as_array().unwrap();
    let changes = nodes
        .iter()
        .any(|review| review["state"] == "CHANGES_REQUESTED");
    let approved = nodes.iter().any(|review| {
        review["state"] == "APPROVED"
            && resource["headRefOid"]
                .as_str()
                .is_some_and(|sha| review["commit"]["oid"] == sha)
    });
    let state = if changes {
        "CHANGES_REQUESTED"
    } else if approved {
        "APPROVED"
    } else if resource["isDraft"] == true {
        "NONE"
    } else {
        "REVIEW_REQUIRED"
    };
    json!({"state":state,"complete":true,"source":"reviews"})
}
fn normalize(resource: &Value, kind: &str, explicit: bool, sha: Option<&str>) -> Value {
    let rollup = &resource["commits"]["nodes"][0]["commit"]["statusCheckRollup"];
    json!({"id":resource["id"],"kind":kind,"number":resource["number"],"title":resource["title"],"url":resource["url"],"state":resource["state"],"draft":resource["isDraft"],"headSha":resource["headRefOid"],"baseBranch":resource["baseRefName"],"explicit":explicit,
        "headMatches":sha.is_some_and(|sha| resource["headRefOid"] == sha),"reviewDecision":resource["reviewDecision"],"reviewSummary":review_summary(resource),
        "checks":if rollup.is_null() { json!({"state":"UNKNOWN","items":[],"complete":false}) } else { json!({"state":rollup["state"],"items":rollup["contexts"]["nodes"],"complete":rollup["contexts"]["pageInfo"]["hasNextPage"] == false}) }})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_origins_and_hosts_are_bounded() {
        for origin in [
            "https://ghe.example.com/team/repo.git",
            "git@ghe.example.com:team/repo.git",
            "ssh://git@ghe.example.com/team/repo.git",
        ] {
            assert_eq!(
                parse_origin(origin),
                Some(Repository {
                    host: "ghe.example.com".into(),
                    owner: "team".into(),
                    name: "repo".into()
                })
            );
        }
        for origin in [
            "git@alias:team/repo",
            "/local/repo",
            "file:///tmp/repo",
            "https://github.com/team/repo/extra",
            "https://github.com/../repo",
            "https://github.com/team/repo?token=x",
            "ssh://git@github.com:22/team/repo",
        ] {
            assert!(parse_origin(origin).is_none(), "{origin}");
        }
        assert_eq!(api_base("github.com"), "https://api.github.com/");
        assert_eq!(
            api_base("ghe.example.com"),
            "https://ghe.example.com/api/v3/"
        );
        assert_eq!(api_base("acme.ghe.com"), "https://api.acme.ghe.com/");
    }
    #[test]
    fn selection_is_scoped_and_never_falls_back() {
        let repo = parse_origin("git@ghe.example.com:t/r").unwrap();
        let mut config = Config::default();
        config.connections.push(Connection {
            host: "github.com".into(),
            login: "public".into(),
            enabled: true,
        });
        assert_eq!(choose(&config, &repo).err(), Some("unconfigured"));
        for login in ["a", "b"] {
            config.connections.push(Connection {
                host: repo.host.clone(),
                login: login.into(),
                enabled: true,
            });
        }
        assert_eq!(choose(&config, &repo).err(), Some("choose_account"));
        config.bindings.insert(repo.key(), "b".into());
        assert_eq!(choose(&config, &repo).unwrap().login, "b");
        config.connections.last_mut().unwrap().enabled = false;
        assert_eq!(choose(&config, &repo).err(), Some("connection_unavailable"));
    }
    #[test]
    fn discovery_drops_all_sensitive_fields() {
        let value = json!({"hosts":{"github.com":[{"login":"alice","state":"success","token":"SECRET","error":"PRIVATE"}],"ghe.example.com":[{"login":"work","state":"error"}]}});
        let serialized = serde_json::to_string(&accounts(&value)).unwrap();
        assert!(!serialized.contains("SECRET"));
        assert!(!serialized.contains("PRIVATE"));
        assert_eq!(accounts(&value).len(), 2);
    }
    #[test]
    fn active_token_path_requires_fresh_matching_identity() {
        let connection = Connection {
            host: "ghe.example.com".into(),
            login: "pinned".into(),
            enabled: true,
        };
        let status = |login: &str, state: &str, active: bool| json!({"hosts":{"ghe.example.com":[{"login":login,"state":state,"active":active}]}});
        assert!(active_account_matches(
            &status("pinned", "success", true),
            &connection
        ));
        assert!(!active_account_matches(
            &status("other", "success", true),
            &connection
        ));
        assert!(!active_account_matches(
            &status("pinned", "error", true),
            &connection
        ));
        assert!(!active_account_matches(
            &status("pinned", "success", false),
            &connection
        ));
        assert!(!active_account_matches(
            &json!({"hosts":{"github.com":[{"login":"pinned","active":true,"state":"success"}]}}),
            &connection
        ));
        assert!(!active_account_matches(
            &json!({"hosts":{"ghe.example.com":[{"login":"pinned","active":true,"state":"success"},{"login":"other","active":true,"state":"success"}]}}),
            &connection
        ));
    }
    #[test]
    fn approval_without_repository_review_rules_is_commit_scoped() {
        let mut pr = json!({"headRefOid":"new","isDraft":false,"reviewDecision":null,"latestOpinionatedReviews":{"pageInfo":{"hasNextPage":false},"nodes":[{"state":"APPROVED","commit":{"oid":"new"}}]}});
        assert_eq!(review_summary(&pr)["state"], "APPROVED");
        pr["latestOpinionatedReviews"]["nodes"][0]["commit"]["oid"] = json!("old");
        assert_eq!(review_summary(&pr)["state"], "REVIEW_REQUIRED");
        pr["latestOpinionatedReviews"]["nodes"][0]["state"] = json!("CHANGES_REQUESTED");
        assert_eq!(review_summary(&pr)["state"], "CHANGES_REQUESTED");
        pr["latestOpinionatedReviews"]["pageInfo"]["hasNextPage"] = json!(true);
        assert_eq!(review_summary(&pr)["state"], "UNKNOWN");
        pr["reviewDecision"] = json!("APPROVED");
        assert_eq!(review_summary(&pr)["state"], "APPROVED");
        pr["reviewDecision"] = Value::Null;
        pr["isDraft"] = json!(true);
        pr["latestOpinionatedReviews"] = json!({"pageInfo":{"hasNextPage":false},"nodes":[]});
        assert_eq!(review_summary(&pr)["state"], "NONE");
    }
    #[test]
    fn checks_are_commit_scoped_and_absent_is_unknown() {
        let pr = json!({"number":1,"headRefOid":"old"});
        let normalized = normalize(&pr, "pullRequest", false, Some("new"));
        assert_eq!(normalized["headMatches"], false);
        assert_eq!(normalized["checks"]["state"], "UNKNOWN");
    }
    #[tokio::test]
    async fn api_fixture_covers_public_enterprise_and_partial_errors() {
        use axum::{
            Router,
            http::HeaderMap,
            routing::{get, post},
        };
        async fn fixture(headers: HeaderMap, Json(input): Json<Value>) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer fixture-only-token");
            let variables = &input["variables"];
            if variables["owner"] == "denied" {
                return Json(
                    json!({"data":{"repository":null},"errors":[{"message":"private server detail"}]}),
                );
            }
            let mut repository = json!({"id":"R_1","nameWithOwner":"team/repo","url":"https://ghe.example.com/team/repo"});
            let pr = json!({"id":"PR_1","number":1,"title":"Fix","url":"https://ghe.example.com/team/repo/pull/1","state":"OPEN","isDraft":false,"headRefOid":"current","headRepository":{"nameWithOwner":"team/repo"},"commits":{"nodes":[{"commit":{"oid":"current","statusCheckRollup":{"state":"FAILURE","contexts":{"pageInfo":{"hasNextPage":false},"nodes":[{"name":"tests","status":"COMPLETED","conclusion":"FAILURE"}]}}}}]}});
            if variables.get("number").is_some() {
                repository["issueOrPullRequest"] = json!({"id":"I_7","number":7,"title":"Issue","state":"OPEN","url":"https://ghe.example.com/team/repo/issues/7"});
            } else if variables.get("branch").is_some() {
                assert_eq!(variables["branch"], "feature/topic");
                let mut fork = pr.clone();
                fork["headRepository"]["nameWithOwner"] = json!("fork/repo");
                fork["number"] = json!(2);
                repository["pullRequests"] =
                    json!({"pageInfo":{"hasNextPage":false},"nodes":[pr,fork]});
            }
            Json(json!({"data":{"repository":repository}}))
        }
        let app = Router::new()
            .route("/graphql", post(fixture))
            .route("/api/graphql", post(fixture))
            .route(
                "/user",
                get(|| async { Json(json!({"login":"fixture-user"})) }),
            )
            .route(
                "/api/v3/user",
                get(|| async { Json(json!({"login":"fixture-user"})) }),
            )
            .route(
                "/limited",
                get(|| async {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "120")],
                        Json(json!({"message":"do not expose this"})),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = octocrab::Octocrab::builder()
            .personal_token("fixture-only-token".to_owned())
            .base_uri(format!("http://{address}/"))
            .unwrap()
            .build()
            .unwrap();
        for host in ["github.com", "ghe.example.com"] {
            let connection = Connection {
                host: host.into(),
                login: "fixture-user".into(),
                enabled: true,
            };
            assert!(verify_identity(&client, &connection).await.is_ok());
            assert!(
                verify_identity(
                    &client,
                    &Connection {
                        login: "different-user".into(),
                        ..connection
                    }
                )
                .await
                .is_err()
            );
            let repo = Repository {
                host: host.into(),
                owner: "team".into(),
                name: "repo".into(),
            };
            let snapshot = fetch_snapshot(
                &client,
                &repo,
                Some("feature/topic"),
                Some("old"),
                &[Link {
                    host: host.into(),
                    owner: "team".into(),
                    repo: "repo".into(),
                    kind: "issue".into(),
                    number: 7,
                }],
            )
            .await
            .unwrap_or_else(|e| panic!("{}", e.message));
            assert_eq!(snapshot["resources"].as_array().unwrap().len(), 2); // fork excluded
            assert_eq!(snapshot["resources"][0]["headMatches"], false);
            assert_eq!(snapshot["resources"][0]["checks"]["state"], "FAILURE");
            assert_eq!(snapshot["resources"][1]["explicit"], true);
            let detached = fetch_snapshot(&client, &repo, None, Some("old"), &[])
                .await
                .unwrap_or_else(|e| panic!("{}", e.message));
            assert_eq!(detached["repository"]["id"], "R_1");
            assert!(detached["resources"].as_array().unwrap().is_empty());
            let denied = Repository {
                owner: "denied".into(),
                ..repo
            };
            let failure = fetch_snapshot(&client, &denied, Some("feature/topic"), Some("old"), &[])
                .await
                .err()
                .unwrap();
            assert!(!failure.message.contains("private server detail"));
        }
        let error = client
            .get::<Value, _, ()>("/limited", None)
            .await
            .err()
            .unwrap();
        let failure = api_error(&error);
        assert!(failure.retry_at >= now() + 119_000);
        assert!(!failure.message.contains("do not expose"));
        server.abort();
    }
}
