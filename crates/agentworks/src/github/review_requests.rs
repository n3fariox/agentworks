//! Explicit repository subscriptions for unattended review automations. Membership
//! is account-private; normalized PR status still feeds the shared status pool.
use super::*;
use crate::automations::GithubReview;

async fn search(client: &octocrab::Octocrab, scope: &GithubReview) -> Result<Value, Failure> {
    let query = format!(
        r#"query($assigned:String!,$requested:String!,$a:String,$b:String) {{
      viewer {{ login }}
      assigned: search(query:$assigned,type:ISSUE,first:100,after:$a) {{ issueCount pageInfo {{ hasNextPage endCursor }} nodes {{ ... on PullRequest {{ {PR_FIELDS} repository {{ name owner {{ login }} }} }} }} }}
      requested: search(query:$requested,type:ISSUE,first:100,after:$b) {{ issueCount pageInfo {{ hasNextPage endCursor }} nodes {{ ... on PullRequest {{ {PR_FIELDS} repository {{ name owner {{ login }} }} }} }} }}
    }}"#
    );
    let mut cursors = [Value::Null, Value::Null];
    let mut done = [false, false];
    let mut counts = [0u64, 0u64];
    let mut rows = serde_json::Map::new();
    for _ in 0..5 {
        let data = graphql(client, &scope.host, query.clone(), json!({
            "assigned":format!("repo:{}/{} is:pr is:open assignee:{}",scope.owner,scope.repo,scope.login),
            "requested":format!("repo:{}/{} is:pr is:open review-requested:{}",scope.owner,scope.repo,scope.login),
            "a":cursors[0],"b":cursors[1]
        })).await?;
        if !data["viewer"]["login"]
            .as_str()
            .is_some_and(|v| v.eq_ignore_ascii_case(&scope.login))
        {
            return Err("GitHub review identity does not match the selected account".into());
        }
        for (i, name) in ["assigned", "requested"].into_iter().enumerate() {
            if done[i] {
                continue;
            }
            let list = &data[name];
            // GitHub search truncation is not an authoritative membership snapshot.
            if list["issueCount"].as_u64().is_none_or(|n| n > 500) {
                return Err("Repository review list is too large or incomplete".into());
            }
            for raw in list["nodes"]
                .as_array()
                .ok_or("Incomplete repository review list")?
            {
                counts[i] += 1;
                if !raw["repository"]["name"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&scope.repo))
                    || !raw["repository"]["owner"]["login"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(&scope.owner))
                {
                    return Err("Unexpected repository in review list".into());
                }
                if raw["state"] != "OPEN" {
                    return Err("Unexpected PR state in review list".into());
                }
                let number = raw["number"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= i32::MAX as u64)
                    .ok_or("Invalid review PR number")?;
                let sha = raw["headRefOid"]
                    .as_str()
                    .filter(|s| {
                        matches!(s.len(), 40 | 64) && s.bytes().all(|c| c.is_ascii_hexdigit())
                    })
                    .ok_or("Missing review PR head SHA")?;
                let row = rows.entry(number.to_string()).or_insert_with(|| {
                    let mut pr = normalize(raw, "pullRequest", false, None);
                    pr["url"] = json!(format!(
                        "https://{}/{}/{}/pull/{number}",
                        scope.host, scope.owner, scope.repo
                    ));
                    json!({"pr":pr,"assigned":false,"requested":false,"headSha":sha})
                });
                row[name] = json!(true);
            }
            let more = list["pageInfo"]["hasNextPage"]
                .as_bool()
                .ok_or("Incomplete review pagination")?;
            done[i] = !more;
            if !more && list["issueCount"].as_u64() != Some(counts[i]) {
                return Err("Incomplete repository review membership".into());
            }
            if more {
                let next = list["pageInfo"]["endCursor"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("Missing review cursor")?;
                if cursors[i] == next {
                    return Err("Repeated review cursor".into());
                }
                cursors[i] = json!(next);
            }
        }
        if done.iter().all(|v| *v) {
            return Ok(Value::Object(rows));
        }
    }
    Err("Repository review list is incomplete; baseline was retained".into())
}

pub(crate) async fn review_requests(app: &App, scope: &GithubReview) -> Result<Value, String> {
    let connection = app
        .catalog
        .lock()
        .unwrap()
        .data
        .github
        .connections
        .iter()
        .find(|c| {
            c.enabled
                && c.host.eq_ignore_ascii_case(&scope.host)
                && c.login.eq_ignore_ascii_case(&scope.login)
        })
        .cloned()
        .ok_or("Enable the selected GitHub account first")?;
    let interval = poll_interval(app, &scope.host);
    let key = json!([scope.host, scope.owner, scope.repo, scope.login]).to_string();
    let account = format!("{}:{}", scope.host, scope.login.to_ascii_lowercase());
    let mut hub = app.github.state.lock().await;
    if let Some((_, value)) = hub
        .review_requests
        .get(&key)
        .filter(|(at, _)| at.elapsed().as_millis() < interval as u128)
    {
        return Ok(value.clone());
    }
    if let Some((_, message)) = hub
        .failures
        .get(&account)
        .filter(|(until, _)| *until > now())
    {
        return Err(message.clone());
    }
    let result = tokio::time::timeout(Duration::from_secs(50), async {
        let client = client(&connection).await?;
        search(&client, scope).await
    })
    .await
    .unwrap_or_else(|_| Err("Repository review refresh timed out".into()));
    match result {
        Ok(value) => {
            let repo = Repository {
                host: scope.host.clone(),
                owner: scope.owner.clone(),
                name: scope.repo.clone(),
            };
            let prs: Vec<_> = value
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .map(|r| r["pr"].clone())
                .collect();
            remember_prs(&mut hub, &repo, &scope.login, &json!({"resources":prs}));
            hub.failures.remove(&account);
            if hub.review_requests.len() >= 200 {
                hub.review_requests.clear();
            }
            hub.review_requests
                .insert(key, (Instant::now(), value.clone()));
            Ok(value)
        }
        Err(error) => {
            hub.failures.insert(
                account,
                (error.retry_at.max(now() + interval), error.message.clone()),
            );
            Err(error.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn repository_membership_is_paginated_identity_checked_and_complete_on_all_hosts() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let mode = Arc::new(AtomicUsize::new(0));
        let state = mode.clone();
        let router=axum::Router::new().route("/{*path}",axum::routing::post(move |Path(path):Path<String>,Json(body):Json<Value>| {
            let mode=state.load(Ordering::SeqCst);
            async move {
                assert!(matches!(path.as_str(),"graphql"|"api/graphql"));
                assert_eq!(body["variables"]["assigned"],"repo:team/repo is:pr is:open assignee:fixture");
                assert_eq!(body["variables"]["requested"],"repo:team/repo is:pr is:open review-requested:fixture");
                let second=!body["variables"]["a"].is_null();
                let pr=|n| json!({"id":format!("PR_{n}"),"number":n,"state":"OPEN","headRefOid":if mode==6 {"invalid".into()} else {"a".repeat(40)},"repository":{"name":if mode==3 {"other"} else {"repo"},"owner":{"login":"team"}}});
                let mut data=json!({"viewer":{"login":if mode==1 {"another"} else {"fixture"}},
                    "assigned":{"issueCount":if mode==4 {501} else if mode==7 {3} else {2},"pageInfo":{"hasNextPage":!second||mode==5,"endCursor":"next"},"nodes":[pr(if second {18} else {17})]},
                    "requested":{"issueCount":1,"pageInfo":{"hasNextPage":false},"nodes":[pr(17)]}});
                if mode==2 { data=json!({}); return Json(json!({"data":data,"errors":[{"message":"private detail"}]})); }
                Json(json!({"data":data}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = octocrab::Octocrab::builder()
            .personal_token("fixture-token".to_owned())
            .base_uri(format!("http://{addr}/"))
            .unwrap()
            .build()
            .unwrap();
        for host in ["github.com", "ghe-a.example.com", "ghe-b.example.com"] {
            let scope = GithubReview {
                host: host.into(),
                owner: "team".into(),
                repo: "repo".into(),
                login: "fixture".into(),
                assigned: true,
                review_requested: true,
                include_existing: false,
            };
            mode.store(0, Ordering::SeqCst);
            let rows = search(&client, &scope)
                .await
                .unwrap_or_else(|e| panic!("{}", e.message));
            assert_eq!(rows.as_object().unwrap().len(), 2);
            assert_eq!(rows["17"]["assigned"], true);
            assert_eq!(rows["17"]["requested"], true);
            assert_eq!(rows["18"]["requested"], false);
            assert_eq!(
                rows["17"]["pr"]["url"],
                format!("https://{host}/team/repo/pull/17")
            );
            for failure in 1..=7 {
                mode.store(failure, Ordering::SeqCst);
                assert!(search(&client, &scope).await.is_err(), "mode {failure}");
            }
        }
        server.abort();
    }
}

#[cfg(test)]
impl Hub {
    pub(crate) async fn seed_review_requests(&self, scope: &GithubReview, value: Value) {
        let key = json!([scope.host, scope.owner, scope.repo, scope.login]).to_string();
        self.state
            .lock()
            .await
            .review_requests
            .insert(key, (Instant::now(), value));
    }
}
