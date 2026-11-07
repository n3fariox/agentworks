//! Local PR-to-session navigation. Reads saved associations without polling GitHub
//! or inspecting local/remote workspaces on the navigation path.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionQuery {
    pub(crate) host: String,
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) number: u64,
}

pub async fn sessions(State(app): State<App>, Query(query): Query<SessionQuery>) -> ApiResult {
    Ok(Json(json!({"sessions":matching_sessions(&app, &query)?})))
}

pub(crate) fn matching_sessions(
    app: &App,
    query: &SessionQuery,
) -> Result<Vec<Value>, (StatusCode, String)> {
    if !valid_host(&query.host)
        || !valid_name(&query.owner)
        || !valid_name(&query.repo)
        || query.number == 0
        || query.number > i32::MAX as u64
    {
        return Err(fail(StatusCode::BAD_REQUEST, "Invalid GitHub PR identity"));
    }
    // Each lock is released before acquiring the next. In particular, never hold
    // the store while reading session metadata (which takes the session lock).
    let catalog = app.catalog.lock().unwrap().data.clone();
    let observed: HashMap<String, i64> = {
        let store = app.store.lock().unwrap();
        let read = || -> rusqlite::Result<HashMap<String, i64>> {
            let mut statement = store.connection.prepare(
                "SELECT session_id,observed_at FROM github_pr_tracking
                 WHERE lower(json_extract(scope,'$[0]'))=lower(?1)
                   AND lower(json_extract(scope,'$[1]'))=lower(?2)
                   AND lower(json_extract(scope,'$[2]'))=lower(?3)
                   AND json_extract(snapshot,?4) IS NOT NULL",
            )?;
            statement
                .query_map(
                    rusqlite::params![
                        query.host,
                        query.owner,
                        query.repo,
                        format!("$.\"{}\"", query.number)
                    ],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                        ))
                    },
                )?
                .collect()
        };
        read().map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not find PR sessions",
            )
        })?
    };
    let sessions: Vec<_> = app.sessions.lock().unwrap().values().cloned().collect();
    let mut matches = Vec::new();
    for session in sessions {
        let attached = catalog.github.links.get(&session.id).is_some_and(|links| {
            links.iter().any(|link| {
                link.kind == "pullRequest"
                    && link.number == query.number
                    && link.host.eq_ignore_ascii_case(&query.host)
                    && link.owner.eq_ignore_ascii_case(&query.owner)
                    && link.repo.eq_ignore_ascii_case(&query.repo)
            })
        });
        if !attached && !observed.contains_key(&session.id) {
            continue;
        }
        let meta = session.meta();
        let project = catalog
            .projects
            .iter()
            .find(|p| Some(p.id.as_str()) == meta["projectId"].as_str());
        let host = catalog.hosts.iter().find(|h| h.id == session.host_id);
        matches.push(json!({"session":meta,"projectName":project.map(|p| &p.name),"hostName":host.map(|h| h.name.as_str()).unwrap_or(if session.host_id == "local" {"Local"} else {&session.host_id}),"association":if attached {"attached"} else {"observed"},"observedAt":observed.get(&session.id)}));
    }
    matches.sort_by(|a, b| {
        b["session"]["created"]
            .as_u64()
            .cmp(&a["session"]["created"].as_u64())
            .then_with(|| {
                a["session"]["id"]
                    .as_str()
                    .cmp(&b["session"]["id"].as_str())
            })
    });
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn query(host: &str, owner: &str, repo: &str, number: u64) -> Query<SessionQuery> {
        Query(SessionQuery {
            host: host.into(),
            owner: owner.into(),
            repo: repo.into(),
            number,
        })
    }
    #[tokio::test]
    async fn attached_and_observed_sessions_are_scoped_deduplicated_and_include_archived() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let mut ids = vec![f.s.id.clone()];
        for title in [
            "Second session",
            "Archived session",
            "Other host",
            "Other repository",
            "Other PR",
            "Issue only",
        ] {
            let req =
                serde_json::from_value(json!({"provider":"demo","cwd":f.s.cwd,"title":title}))
                    .unwrap();
            let Json(created) = crate::create_session(State(f.app.clone()), Json(req))
                .await
                .unwrap();
            ids.push(created["id"].as_str().unwrap().to_owned());
        }
        let mut link = Link {
            host: "github.com".into(),
            owner: "team".into(),
            repo: "repo".into(),
            kind: "pullRequest".into(),
            number: 17,
        };
        {
            let mut catalog = f.app.catalog.lock().unwrap();
            catalog.data.projects.push(
                serde_json::from_value(json!({"id":"project","name":"Review project"})).unwrap(),
            );
            f.s.data.lock().unwrap().project_id = Some("project".into());
            // An explicit attachment is immediately navigable before any API observation.
            catalog
                .data
                .github
                .links
                .insert(ids[0].clone(), vec![link.clone()]);
            catalog
                .data
                .github
                .links
                .insert(ids[2].clone(), vec![link.clone()]);
            link.kind = "issue".into();
            catalog.data.github.links.insert(ids[6].clone(), vec![link]);
        }
        let archived = f.app.sessions.lock().unwrap().get(&ids[2]).unwrap().clone();
        archived.data.lock().unwrap().lifecycle.archived = true;
        {
            let store = f.app.store.lock().unwrap();
            for (id, host, repo, number) in [
                (&ids[0], "github.com", "repo", 17),
                (&ids[1], "github.com", "repo", 17),
                (&ids[3], "ghe.example.com", "repo", 17),
                (&ids[4], "github.com", "other", 17),
                (&ids[5], "github.com", "repo", 18),
            ] {
                store.connection.execute("INSERT INTO github_pr_tracking(session_id,scope,snapshot,status,observed_at) VALUES (?1,?2,?3,'unavailable',123)",rusqlite::params![id,json!([host,"team",repo,"other-account","feature/topic"]).to_string(),json!({number.to_string():{"number":number}}).to_string()]).unwrap();
            }
        }
        let Json(value) = sessions(
            State(f.app.clone()),
            query("GITHUB.COM", "TEAM", "REPO", 17),
        )
        .await
        .unwrap();
        let rows = value["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter()
                .any(|r| r["session"]["id"] == ids[2] && r["session"]["archived"] == true)
        );
        let attached = rows.iter().find(|r| r["session"]["id"] == ids[0]).unwrap();
        assert_eq!(attached["association"], "attached");
        assert_eq!(attached["projectName"], "Review project");
        let tracked = rows.iter().find(|r| r["session"]["id"] == ids[1]).unwrap();
        assert_eq!(tracked["association"], "observed");
        assert_eq!(tracked["observedAt"], 123);
        assert_eq!(tracked["hostName"], "Local");
        assert!(
            sessions(State(f.app.clone()), query("github.com", "team", "repo", 0))
                .await
                .is_err()
        );
        assert!(
            sessions(
                State(f.app.clone()),
                query("github.com", "team", "../repo", 17)
            )
            .await
            .is_err()
        );
        // Explicit attachments still work when tracking has no matching record.
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute("DELETE FROM github_pr_tracking", [])
            .unwrap();
        let Json(value) = sessions(
            State(f.app.clone()),
            query("github.com", "team", "repo", 17),
        )
        .await
        .unwrap();
        assert_eq!(value["sessions"].as_array().unwrap().len(), 2);
        f.app
            .catalog
            .lock()
            .unwrap()
            .data
            .github
            .links
            .remove(&ids[0]);
        let Json(value) = sessions(
            State(f.app.clone()),
            query("github.com", "team", "repo", 17),
        )
        .await
        .unwrap();
        assert_eq!(value["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(value["sessions"][0]["session"]["id"], ids[2]);
        // Deleted sessions are excluded even if a saved attachment remains.
        f.app.sessions.lock().unwrap().remove(&ids[2]);
        let Json(value) = sessions(
            State(f.app.clone()),
            query("github.com", "team", "repo", 17),
        )
        .await
        .unwrap();
        assert_eq!(value["sessions"], json!([]));
    }
}
