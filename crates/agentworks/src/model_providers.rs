//! One provider definition, with explicit availability on execution hosts.
use crate::{ApiResult, App, fail};
use agentworks_runtime::model_providers as profiles;
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostQuery {
    pub host_id: Option<String>,
}

pub async fn list(State(_app): State<App>, Query(query): Query<HostQuery>) -> ApiResult {
    let mut result = profiles::dispatch(&json!({"operation":"list"})).map_err(bad)?;
    if let Some(host) = query.host_id {
        result["profiles"].as_array_mut().unwrap().retain(|p| {
            p["hostIds"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == &host))
        });
    }
    Ok(Json(result))
}
fn bad(message: impl Into<String>) -> (StatusCode, String) {
    fail(StatusCode::BAD_REQUEST, message)
}

fn validate_hosts(app: &App, request: &Value) -> Result<(), (StatusCode, String)> {
    let Some(ids) = request["profile"].get("hostIds") else {
        return Ok(());
    };
    let ids = ids
        .as_array()
        .ok_or_else(|| bad("Choose available hosts"))?;
    let catalog = app.catalog.lock().unwrap();
    for id in ids {
        let id = id.as_str().ok_or_else(|| bad("Invalid host ID"))?;
        if id != "local" && !catalog.data.hosts.iter().any(|h| h.id == id) {
            return Err(bad("Choose hosts registered in Agentworks"));
        }
    }
    Ok(())
}

pub async fn selected(
    _app: &App,
    host: &str,
    provider: &str,
    id: Option<&str>,
) -> Result<Option<Value>, (StatusCode, String)> {
    let Some(id) = id else {
        return Ok(None);
    };
    if provider != "goose" {
        return Err(bad("Model provider profiles require Goose"));
    }
    profiles::authorize(id, host).map_err(bad)?;
    let all = profiles::dispatch(&json!({"operation":"list"})).map_err(bad)?;
    Ok(all["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .cloned())
}

pub async fn update(
    State(app): State<App>,
    Query(query): Query<HostQuery>,
    Json(request): Json<Value>,
) -> ApiResult {
    let _workspace = app.workspace_lock.lock().await;
    let operation = request["operation"].as_str().unwrap_or("");
    if matches!(operation, "save" | "test" | "models") {
        validate_hosts(&app, &request)?;
    }
    if matches!(operation, "test" | "models") {
        let host = query.host_id.as_deref().unwrap_or("local");
        let connection = if request["profile"].is_object() {
            profiles::draft_connection(&request, host).map_err(bad)?
        } else {
            profiles::connection(
                request["id"]
                    .as_str()
                    .ok_or_else(|| bad("Select a provider"))?,
                host,
            )
            .map_err(bad)?
        };
        let private = connection.test_request(operation);
        let result = if host == "local" {
            profiles::execute(&private).await.map_err(bad)?
        } else {
            app.hosts
                .live(host)
                .ok_or_else(|| {
                    fail(
                        StatusCode::CONFLICT,
                        "Connect this host before testing its connection",
                    )
                })?
                .model_providers(private)
                .await
                .map_err(bad)?
        };
        return Ok(Json(result));
    }
    let id = if operation == "save" {
        request["profile"]["id"].as_str()
    } else {
        request["id"].as_str()
    };
    if let Some(id) = id {
        let used = app
            .sessions
            .lock()
            .unwrap()
            .values()
            .any(|s| s.data.lock().unwrap().model_provider.as_deref() == Some(id));
        if operation == "delete"
            && app
                .store
                .lock()
                .unwrap()
                .automations()
                .map_err(|_| bad("Could not read automations"))?
                .iter()
                .any(|d| d.model_provider.as_deref() == Some(id))
        {
            return Err(fail(
                StatusCode::CONFLICT,
                "This provider is used by automations. Choose another provider in those automations before deleting it",
            ));
        }
        if used && operation == "delete" {
            return Err(fail(
                StatusCode::CONFLICT,
                "This provider is used by saved sessions. Delete those sessions before deleting the provider",
            ));
        }
        if used && operation == "save" {
            let all = profiles::dispatch(&json!({"operation":"list"})).map_err(bad)?;
            let old = all["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == id)
                .ok_or_else(|| bad("Provider no longer exists"))?;
            let proposed = request["profile"]["baseUrl"]
                .as_str()
                .unwrap_or("")
                .trim()
                .trim_end_matches('/');
            if old["baseUrl"] != proposed
                || old["kind"].as_str().unwrap_or("openai-compatible")
                    != request["profile"]["kind"]
                        .as_str()
                        .unwrap_or("openai-compatible")
            {
                return Err(fail(
                    StatusCode::CONFLICT,
                    "Saved sessions use this endpoint. Add another provider to use a different URL",
                ));
            }
        }
    }
    if operation == "delete" {
        if let Some(id) = id {
            crate::terminal::remove_provider_auth(id).await;
            crate::codex_oauth::remove(id).await;
        }
    }
    Ok(Json(profiles::dispatch(&request).map_err(bad)?))
}
