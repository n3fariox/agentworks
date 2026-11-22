//! Authored/assigned/review-requested open PR membership, fetched only by active dashboard reads.
use super::*;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Polling {
    host: String,
    interval_seconds: u64,
}
pub async fn save_polling(State(app): State<App>, Json(mut input): Json<Polling>) -> ApiResult {
    input.host = input.host.to_ascii_lowercase();
    if !valid_host(&input.host) || !(60..=3600).contains(&input.interval_seconds) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Choose a configured host and a polling interval from 1 to 60 minutes",
        ));
    }
    let hub = app.github.state.lock().await;
    let mut catalog = app.catalog.lock().unwrap();
    let mut config = catalog.data.github.clone();
    if !config.connections.iter().any(|c| c.host == input.host)
        && !hub
            .discovery
            .as_ref()
            .and_then(|(_, result)| result.as_ref().ok())
            .is_some_and(|rows| rows.iter().any(|a| a.host == input.host))
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Rediscover GitHub accounts or configure this host first",
        ));
    }
    config
        .poll_intervals
        .insert(input.host.clone(), input.interval_seconds);
    catalog.github(config).map_err(|_| {
        fail(
            StatusCode::INSUFFICIENT_STORAGE,
            "Could not save GitHub polling settings",
        )
    })?;
    drop(catalog);
    app.store
        .lock()
        .unwrap()
        .connection
        .execute("UPDATE github_pr_tracking SET next_poll=0 WHERE json_extract(scope,'$[0]')=?1 OR scope IS NULL", [&input.host])
        .map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not reschedule GitHub polling",
            )
        })?;
    Ok(Json(
        json!({"host":input.host,"intervalSeconds":input.interval_seconds}),
    ))
}
fn account_key(connection: &Connection) -> String {
    format!(
        "{}:{}",
        connection.host,
        connection.login.to_ascii_lowercase()
    )
}
// Account-private review state stays on the membership row, outside the shared
// PR status pool: a session refresh must not erase another account's reminder.
const PENDING_REVIEW_FIELDS: &str = r#"
 pendingReview: reviews(first:1, author:$login, states:[PENDING]) {
   totalCount nodes { state author { login } comments { totalCount } }
 }
"#;
fn pending_reviews(raw: &Value, login: &str) -> Value {
    let mut result = json!({"complete":false,"accounts":[],"fetchedAt":now(),"stale":false});
    let reviews = &raw["pendingReview"];
    let (Some(total), Some(nodes)) = (reviews["totalCount"].as_u64(), reviews["nodes"].as_array())
    else {
        return result;
    };
    if nodes.len() as u64 > total || (total > 0 && nodes.is_empty()) {
        return result;
    }
    let mut comments = 0u64;
    for review in nodes {
        if review["state"] != "PENDING"
            || !review["author"]["login"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(login))
        {
            return result;
        }
        let Some(count) = review["comments"]["totalCount"].as_u64() else {
            return result;
        };
        comments = comments.saturating_add(count);
    }
    result["complete"] = json!(total == nodes.len() as u64);
    if total > 0 {
        result["accounts"] = json!([{"login":login,"commentCount":comments}]);
    }
    result
}
fn merge_pending_reviews(a: &Value, b: &Value) -> Value {
    let mut accounts: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for value in [a, b] {
        for account in value["accounts"].as_array().into_iter().flatten() {
            if let Some(login) = account["login"].as_str() {
                accounts.insert(login.to_ascii_lowercase(), account.clone());
            }
        }
    }
    json!({"complete":a["complete"] == true && b["complete"] == true,
        "stale":a["stale"] == true || b["stale"] == true,
        "fetchedAt":a["fetchedAt"].as_u64().unwrap_or_default().min(b["fetchedAt"].as_u64().unwrap_or_default()),
        "accounts":accounts.into_values().collect::<Vec<_>>()})
}
async fn search(client: &octocrab::Octocrab, connection: &Connection) -> Result<Value, Failure> {
    // Separate searches make authored OR assigned OR review-requested explicit on public and GHE hosts.
    let query = format!(
        r#"query($authored:String!,$assigned:String!,$reviewRequested:String!,$login:String!,$a:String,$b:String,$c:String) {{
      viewer {{ login }}
      authored: search(query:$authored,type:ISSUE,first:100,after:$a) {{ issueCount pageInfo {{ hasNextPage endCursor }} nodes {{ ... on PullRequest {{ {PR_FIELDS} {PENDING_REVIEW_FIELDS} repository {{ name owner {{ login }} }} }} }} }}
      assigned: search(query:$assigned,type:ISSUE,first:100,after:$b) {{ issueCount pageInfo {{ hasNextPage endCursor }} nodes {{ ... on PullRequest {{ {PR_FIELDS} {PENDING_REVIEW_FIELDS} repository {{ name owner {{ login }} }} }} }} }}
      reviewRequested: search(query:$reviewRequested,type:ISSUE,first:100,after:$c) {{ issueCount pageInfo {{ hasNextPage endCursor }} nodes {{ ... on PullRequest {{ {PR_FIELDS} {PENDING_REVIEW_FIELDS} repository {{ name owner {{ login }} }} }} }} }}
    }}"#
    );
    let mut a = Value::Null;
    let mut b = Value::Null;
    let mut c = Value::Null;
    let mut done = [false, false, false];
    let mut rows: HashMap<String, Value> = HashMap::new();
    let mut complete = false;
    for _ in 0..5 {
        let data=graphql(client,&connection.host,query.clone(),json!({"login":connection.login,"authored":format!("is:pr is:open author:{} sort:updated-desc",connection.login),"assigned":format!("is:pr is:open assignee:{} sort:updated-desc",connection.login),"reviewRequested":format!("is:pr is:open review-requested:{} sort:updated-desc",connection.login),"a":a,"b":b,"c":c})).await?;
        if !data["viewer"]["login"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(&connection.login))
        {
            return Err("GitHub search identity does not match the selected account".into());
        }
        for (index, name) in ["authored", "assigned", "reviewRequested"]
            .iter()
            .enumerate()
        {
            if done[index] {
                continue;
            }
            let list = &data[*name];
            let nodes = list["nodes"]
                .as_array()
                .ok_or("GitHub returned an incomplete PR list")?;
            for raw in nodes {
                if raw["state"] != "OPEN" {
                    continue;
                }
                let owner = raw["repository"]["owner"]["login"]
                    .as_str()
                    .filter(|s| valid_name(s))
                    .ok_or("GitHub returned an invalid PR repository")?;
                let repo = raw["repository"]["name"]
                    .as_str()
                    .filter(|s| valid_name(s))
                    .ok_or("GitHub returned an invalid PR repository")?;
                let number = raw["number"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or("GitHub returned an invalid PR number")?;
                let key = format!("{owner}/{repo}/{number}").to_ascii_lowercase();
                let row=rows.entry(key).or_insert_with(|| {
                    let mut resource=normalize(raw,"pullRequest",false,None);
                    // Scope the destination to the authenticated host and validated repository.
                    resource["url"]=json!(format!("https://{}/{owner}/{repo}/pull/{number}",connection.host));
                    resource["title"]=json!(raw["title"].as_str().unwrap_or_default().chars().take(256).collect::<String>());
                    json!({"repository":{"host":connection.host,"owner":owner,"name":repo},"pr":resource,"authored":false,"assigned":false,"reviewRequested":false,"login":connection.login,"pendingReviews":pending_reviews(raw,&connection.login)})
                });
                row[*name] = json!(true);
            }
            let more = list["pageInfo"]["hasNextPage"]
                .as_bool()
                .ok_or("GitHub returned incomplete PR pagination")?;
            done[index] = !more;
            if more {
                let next = list["pageInfo"]["endCursor"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("GitHub omitted a PR page cursor")?;
                let cursor = match index {
                    0 => &mut a,
                    1 => &mut b,
                    _ => &mut c,
                };
                if *cursor == next {
                    return Err("GitHub repeated a PR page cursor".into());
                }
                *cursor = json!(next);
            }
        }
        if done.iter().all(|d| *d) {
            complete = true;
            break;
        }
    }
    let mut rows: Vec<_> = rows.into_values().collect();
    rows.sort_by(|a, b| {
        b["pr"]["updatedAt"]
            .as_str()
            .cmp(&a["pr"]["updatedAt"].as_str())
    });
    Ok(json!({"rows":rows,"complete":complete,"fetchedAt":now()}))
}
pub async fn pulls(State(app): State<App>) -> ApiResult {
    let config = app.catalog.lock().unwrap().data.github.clone();
    let mut hub = app.github.state.lock().await;
    let mut hosts: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for connection in config.connections.iter().filter(|c| c.enabled) {
        let interval = config
            .poll_intervals
            .get(&connection.host)
            .copied()
            .unwrap_or(300)
            .clamp(60, 3600)
            * 1000;
        let key = account_key(connection);
        let due = hub
            .browser
            .get(&key)
            .is_none_or(|(at, _)| at.elapsed().as_millis() >= interval as u128);
        let mut error = hub.failures.get(&key).map(|(_, message)| message.clone());
        let mut retry_at = 0;
        if due {
            if let Some((until, message)) =
                hub.failures.get(&key).filter(|(until, _)| *until > now())
            {
                error = Some(message.clone());
                retry_at = *until;
            } else {
                let fetched = tokio::time::timeout(Duration::from_secs(50), async {
                    let client = client(connection).await?;
                    search(&client, connection).await
                })
                .await
                .unwrap_or_else(|_| Err("GitHub PR list refresh timed out".into()));
                match fetched {
                    Ok(value) => {
                        error = None;
                        hub.failures.remove(&key);
                        if hub.browser.len() >= 40 && !hub.browser.contains_key(&key) {
                            hub.browser.clear();
                        }
                        for row in value["rows"].as_array().into_iter().flatten() {
                            let repo: Repository = serde_json::from_value(
                                row["repository"].clone(),
                            )
                            .map_err(|_| {
                                fail(
                                    StatusCode::BAD_GATEWAY,
                                    "GitHub returned an invalid repository",
                                )
                            })?;
                            remember_prs(
                                &mut hub,
                                &repo,
                                &connection.login,
                                &json!({"resources":[row["pr"]]}),
                            );
                        }
                        hub.browser.insert(key.clone(), (Instant::now(), value));
                    }
                    Err(failure) => {
                        error = Some(failure.message.clone());
                        retry_at = failure.retry_at.max(now() + interval);
                        hub.failures
                            .insert(key.clone(), (retry_at, failure.message));
                    }
                }
            }
        }
        let host=hosts.entry(connection.host.clone()).or_insert_with(||json!({"host":connection.host,"intervalMs":interval,"accounts":[],"rows":[],"messages":[],"nextPollAt":u64::MAX}));
        host["accounts"]
            .as_array_mut()
            .unwrap()
            .push(json!(connection.login));
        if let Some(message) = &error {
            host["messages"]
                .as_array_mut()
                .unwrap()
                .push(json!({"login":connection.login,"message":message}));
        }
        let cached = hub.browser.get(&key);
        let next = cached
            .map(|(at, _)| now() + interval.saturating_sub(at.elapsed().as_millis() as u64))
            .unwrap_or(now() + interval)
            .max(retry_at);
        host["nextPollAt"] = json!(host["nextPollAt"].as_u64().unwrap_or(next).min(next));
        if let Some((at, value)) = cached {
            if value["complete"] != true {
                host["messages"].as_array_mut().unwrap().push(json!({"login":connection.login,"message":"Showing up to 500 PRs per category (created, assigned, review requested); more results exist"}));
            }
            for row in value["rows"].as_array().into_iter().flatten() {
                let mut row = row.clone();
                let repo: Repository = serde_json::from_value(row["repository"].clone()).unwrap();
                if let Some((_, pr)) =
                    hub.prs
                        .get(&pr_key(&repo, &connection.login, &row["pr"]["number"]))
                {
                    row["pr"] = pr.clone();
                }
                // A shared status update can close a PR before the next membership search.
                if row["pr"]["state"] != "OPEN" {
                    continue;
                }
                let mut badge = crate::session_badges::pr_badge(&row["pr"], false);
                let stale = error.is_some() || at.elapsed().as_millis() >= 2 * interval as u128;
                if stale {
                    badge.tone = "neutral".into();
                    badge
                        .tooltip
                        .push_str(" · Status unavailable or out of date");
                }
                if row["pendingReviews"].is_null() {
                    row["pendingReviews"] = pending_reviews(&Value::Null, &connection.login);
                }
                row["pendingReviews"]["stale"] = json!(stale);
                if stale {
                    row["pendingReviews"]["complete"] = json!(false);
                }
                row["badge"] = json!(badge);
                row["fetchedAt"] = value["fetchedAt"].clone();
                row["stale"] = json!(stale);
                host["rows"].as_array_mut().unwrap().push(row);
            }
        }
    }
    // A PR visible through several selected identities occupies one row per host.
    for host in hosts.values_mut() {
        let rows = std::mem::take(host["rows"].as_array_mut().unwrap());
        let mut unique: std::collections::BTreeMap<String, Value> =
            std::collections::BTreeMap::new();
        for row in rows {
            let key = json!([
                row["repository"]["owner"],
                row["repository"]["name"],
                row["pr"]["number"]
            ])
            .to_string();
            if let Some(old) = unique.get_mut(&key) {
                let authored = old["authored"] == true || row["authored"] == true;
                let assigned = old["assigned"] == true || row["assigned"] == true;
                let review_requested =
                    old["reviewRequested"] == true || row["reviewRequested"] == true;
                let pending = merge_pending_reviews(&old["pendingReviews"], &row["pendingReviews"]);
                if old["stale"] == true && row["stale"] == false {
                    *old = row;
                }
                old["authored"] = json!(authored);
                old["assigned"] = json!(assigned);
                old["reviewRequested"] = json!(review_requested);
                old["pendingReviews"] = pending;
            } else {
                unique.insert(key, row);
            }
        }
        host["rows"] = json!(unique.into_values().collect::<Vec<_>>());
    }
    Ok(Json(
        json!({"hosts":hosts.into_values().collect::<Vec<_>>(),"defaultIntervalMs":DEFAULT_POLL_MS,"credentialHost":"local"}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw(number: u64) -> Value {
        let mut pr = json!({"id":format!("pr-{number}"),"number":number,"title":"<b>PR title</b>","url":"https://ignored.invalid","state":"OPEN","isDraft":false,"headRefName":"feature/topic","headRefOid":"a".repeat(40),"headRepository":{"nameWithOwner":"team/repo"},"baseRefName":"main","author":{"login":"fixture"},"reviewDecision":"APPROVED","repository":{"name":"repo","owner":{"login":"team"}},"commits":{"nodes":[{"commit":{"oid":"a".repeat(40),"statusCheckRollup":{"state":"SUCCESS","contexts":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}}]}});
        pr["pendingReview"] = if number == 1 {
            json!({"totalCount":1,"nodes":[{"state":"PENDING","author":{"login":"fixture"},"comments":{"totalCount":3}}]})
        } else {
            json!({"totalCount":0,"nodes":[]})
        };
        pr
    }
    #[tokio::test]
    async fn all_categories_are_identity_checked_and_deduplicated_on_all_hosts() {
        let wrong = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mode = wrong.clone();
        let submitted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let submitted_mode = submitted.clone();
        let router=axum::Router::new().route("/{*path}",axum::routing::post(move |Path(path):Path<String>,Json(body):Json<Value>|{let wrong=mode.load(std::sync::atomic::Ordering::SeqCst);let submitted=submitted_mode.load(std::sync::atomic::Ordering::SeqCst);async move {
            let pr = |number| { let mut value=raw(number); if submitted { value["pendingReview"]=json!({"totalCount":0,"nodes":[]}); } value };
            assert_eq!(body["variables"]["login"],"fixture");assert!(body["query"].as_str().unwrap().contains("author:$login, states:[PENDING]"));
            assert!(matches!(path.as_str(),"graphql"|"api/graphql"));assert_eq!(body["variables"]["authored"],"is:pr is:open author:fixture sort:updated-desc");assert_eq!(body["variables"]["assigned"],"is:pr is:open assignee:fixture sort:updated-desc");assert_eq!(body["variables"]["reviewRequested"],"is:pr is:open review-requested:fixture sort:updated-desc");
            Json(json!({"data":{"viewer":{"login":if wrong {"another"} else {"fixture"}},"authored":{"issueCount":1,"pageInfo":{"hasNextPage":false},"nodes":[pr(1)]},"assigned":{"issueCount":2,"pageInfo":{"hasNextPage":false},"nodes":[pr(1),pr(2)]},"reviewRequested":{"issueCount":2,"pageInfo":{"hasNextPage":false},"nodes":[pr(1),pr(3)]}}}))
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = octocrab::Octocrab::builder()
            .personal_token("fixture-only".to_owned())
            .base_uri(format!("http://{addr}/"))
            .unwrap()
            .build()
            .unwrap();
        for host in ["github.com", "ghe-a.example.com", "ghe-b.example.com"] {
            let connection = Connection {
                host: host.into(),
                login: "fixture".into(),
                enabled: true,
            };
            let value = search(&client, &connection)
                .await
                .unwrap_or_else(|e| panic!("{}", e.message));
            assert_eq!(value["rows"].as_array().unwrap().len(), 3);
            let both = value["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["pr"]["number"] == 1)
                .unwrap();
            assert_eq!(both["authored"], true);
            assert_eq!(both["assigned"], true);
            assert_eq!(both["reviewRequested"], true);
            assert_eq!(
                both["pendingReviews"]["accounts"],
                json!([{"login":"fixture","commentCount":3}])
            );
            assert_eq!(both["pendingReviews"]["complete"], true);
            let requested = value["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["pr"]["number"] == 3)
                .unwrap();
            assert_eq!(requested["reviewRequested"], true);
            assert_eq!(requested["authored"], false);
            assert_eq!(requested["assigned"], false);
            assert_eq!(
                both["pr"]["url"],
                format!("https://{host}/team/repo/pull/1")
            );
        }
        submitted.store(true, std::sync::atomic::Ordering::SeqCst);
        let cleared = search(
            &client,
            &Connection {
                host: "github.com".into(),
                login: "fixture".into(),
                enabled: true,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{}", e.message));
        assert!(
            cleared["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["pendingReviews"]["accounts"] == json!([])
                    && r["pendingReviews"]["complete"] == true)
        );
        wrong.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            search(
                &client,
                &Connection {
                    host: "github.com".into(),
                    login: "fixture".into(),
                    enabled: true
                }
            )
            .await
            .is_err()
        );
        server.abort();
    }
    #[test]
    fn pending_reviews_are_personal_and_unknown_is_not_a_clear_result() {
        let pending = pending_reviews(&raw(1), "FIXTURE");
        assert_eq!(pending["accounts"][0]["commentCount"], 3);
        assert_eq!(pending["complete"], true);
        assert_eq!(pending_reviews(&raw(1), "someone-else")["complete"], false);
        assert_eq!(pending_reviews(&Value::Null, "fixture")["complete"], false);
        let mut body_only = raw(1);
        body_only["pendingReview"]["nodes"][0]["comments"]["totalCount"] = json!(0);
        assert_eq!(
            pending_reviews(&body_only, "fixture")["accounts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut submitted = raw(1);
        submitted["pendingReview"]["nodes"][0]["state"] = json!("COMMENTED");
        assert_eq!(
            pending_reviews(&submitted, "fixture")["accounts"],
            json!([])
        );
        assert_eq!(pending_reviews(&submitted, "fixture")["complete"], false);
    }
    #[tokio::test]
    async fn pending_reminders_survive_account_dedup_pool_overlays_and_outages() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let repo = Repository {
            host: "github.com".into(),
            owner: "team".into(),
            name: "repo".into(),
        };
        f.app
            .catalog
            .lock()
            .unwrap()
            .data
            .github
            .connections
            .push(Connection {
                host: repo.host.clone(),
                login: "second".into(),
                enabled: true,
            });
        {
            let mut hub = f.app.github.state.lock().await;
            for login in ["fixture", "second"] {
                let connection = Connection {
                    host: repo.host.clone(),
                    login: login.into(),
                    enabled: true,
                };
                let mut reminder = pending_reviews(&raw(1), "fixture");
                reminder["accounts"][0]["login"] = json!(login);
                let pr = normalize(&raw(1), "pullRequest", false, None);
                hub.browser.insert(account_key(&connection),(Instant::now(),json!({"rows":[{"repository":repo,"pr":pr,"authored":true,"assigned":false,"reviewRequested":false,"pendingReviews":reminder}],"complete":true,"fetchedAt":now()})));
                // Session status has no private review data and must not erase it.
                remember_prs(&mut hub, &repo, login, &json!({"resources":[pr]}));
            }
            hub.failures.insert(
                "github.com:fixture".into(),
                (now() + 300000, "Fixture outage".into()),
            );
        }
        let Json(result) = pulls(State(f.app.clone())).await.unwrap();
        let rows = result["hosts"][0]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["pendingReviews"]["accounts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(rows[0]["pendingReviews"]["complete"], false);
        assert_eq!(rows[0]["pendingReviews"]["stale"], true);
    }
    #[tokio::test]
    async fn host_cadence_cache_and_shared_pr_records_serve_both_consumers() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let connection = Connection {
            host: "ghe-a.example.com".into(),
            login: "fixture".into(),
            enabled: true,
        };
        f.app
            .catalog
            .lock()
            .unwrap()
            .data
            .github
            .connections
            .push(connection.clone());
        let _ = save_polling(
            State(f.app.clone()),
            Json(Polling {
                host: connection.host.clone(),
                interval_seconds: 60,
            }),
        )
        .await
        .unwrap();
        assert_eq!(poll_interval(&f.app, "github.com"), 300000);
        assert_eq!(poll_interval(&f.app, &connection.host), 60000);
        {
            let mut hub = f.app.github.state.lock().await;
            for host in ["github.com", "ghe-a.example.com"] {
                let connection = Connection {
                    host: host.into(),
                    login: "fixture".into(),
                    enabled: true,
                };
                let pr = normalize(&raw(1), "pullRequest", false, None);
                hub.browser.insert(account_key(&connection),(Instant::now()-Duration::from_secs(61),json!({"rows":[{"repository":{"host":host,"owner":"team","name":"repo"},"pr":pr,"authored":true,"assigned":false,"login":"fixture"}],"fetchedAt":now()-61000,"complete":true})));
            }
            hub.failures.insert(
                account_key(&connection),
                (now() + 60000, "Fixture rate limit".into()),
            );
        }
        let Json(value) = pulls(State(f.app.clone())).await.unwrap();
        assert_eq!(value["hosts"].as_array().unwrap().len(), 2);
        let public = value["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["host"] == "github.com")
            .unwrap();
        assert_eq!(public["rows"][0]["badge"]["tone"], "success");
        let internal = value["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["host"] == "ghe-a.example.com")
            .unwrap();
        assert_eq!(internal["rows"][0]["badge"]["tone"], "neutral");
        assert_eq!(internal["messages"][0]["message"], "Fixture rate limit");
        let mut changed = normalize(&raw(1), "pullRequest", false, None);
        changed["reviewSummary"]["state"] = json!("CHANGES_REQUESTED");
        changed["checks"]["state"] = json!("FAILURE");
        remember_prs(
            &mut *f.app.github.state.lock().await,
            &Repository {
                host: "github.com".into(),
                owner: "team".into(),
                name: "repo".into(),
            },
            "fixture",
            &json!({"resources":[changed]}),
        );
        let Json(value) = pulls(State(f.app.clone())).await.unwrap();
        let public = value["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["host"] == "github.com")
            .unwrap();
        assert_eq!(public["rows"][0]["badge"]["tone"], "danger");
        let mut snapshot = json!({"resources":[normalize(&raw(1),"pullRequest",false,None)]});
        overlay_prs(
            &*f.app.github.state.lock().await,
            &Repository {
                host: "github.com".into(),
                owner: "team".into(),
                name: "repo".into(),
            },
            "fixture",
            &mut snapshot,
            300000,
        );
        assert_eq!(snapshot["resources"][0]["checks"]["state"], "FAILURE");
        assert!(
            save_polling(
                State(f.app.clone()),
                Json(Polling {
                    host: "github.com".into(),
                    interval_seconds: 5
                })
            )
            .await
            .is_err()
        );
    }
}
