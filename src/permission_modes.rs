//! Explicit per-session permission-mode selection through the harness config API.
use crate::{ApiResult, App, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn parse(config: &Value) -> Option<Value> {
    let option = config["configOptions"]
        .as_array()?
        .iter()
        .find(|o| o["category"] == "mode" || o["id"] == "mode")?;
    let id = option["id"].as_str()?;
    let current = option["currentValue"].as_str()?;
    let options: Vec<Value> = option["options"]
        .as_array()?
        .iter()
        .filter(|o| o["value"].is_string())
        .cloned()
        .collect();
    if !options.iter().any(|o| o["value"] == current) {
        return None;
    }
    Some(json!({"configId":id,"currentValue":current,"options":options}))
}

pub async fn get(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let config = s.data.lock().unwrap().permission_modes.clone();
    Ok(Json(config))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    mode: String,
}

pub async fn set(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Choice>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    crate::service_restart::allow_start(&app)?;
    let s = session(&app, &id)?;
    {
        let mut data = s.data.lock().unwrap();
        if data.status != "idle" {
            return Err(fail(
                StatusCode::CONFLICT,
                "Choose a permission mode while the session is idle",
            ));
        }
        if !data.permission_modes["options"]
            .as_array()
            .is_some_and(|options| options.iter().any(|o| o["value"] == req.mode))
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Choose one of this session's advertised permission modes",
            ));
        }
        data.status = "configuring".into();
    }
    s.emit("status", json!({"status":"configuring"}));
    if let Err(error) = s.send_command(crate::runtime::Command::PermissionMode(req.mode)) {
        s.status("error");
        return Err(error);
    }
    Ok(Json(json!({"accepted":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_advertised_modes_and_actual_fallback() {
        let config = json!({"configOptions":[{"id":"model","category":"model","currentValue":"x","options":[{"value":"x"}]},{"id":"mode","category":"mode","currentValue":"acceptEdits","options":[{"value":"default"},{"value":"auto"},{"value":"acceptEdits"}]}]});
        let parsed = parse(&config).unwrap();
        assert_eq!(parsed["currentValue"], "acceptEdits");
        assert_eq!(parsed["options"].as_array().unwrap().len(), 3);
        assert!(parse(&json!({"configOptions":[{"id":"mode","currentValue":"invented","options":[{"value":"auto"}]}]})).is_none());
        assert!(parse(&json!({"configOptions":[]})).is_none());
    }
}
