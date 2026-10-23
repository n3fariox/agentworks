//! Provider-advertised session settings and ACP v1 mode compatibility.
use crate::{ApiResult, App, Session, fail, session};
use agent_client_protocol::schema::v1::{
    SessionConfigOptionValue, SetSessionConfigOptionRequest, SetSessionModeRequest,
};
use agent_client_protocol::{Agent, ConnectionTo};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub fn choices(option: &Value) -> Vec<Value> {
    fn collect(items: &[Value], out: &mut Vec<Value>) {
        for item in items {
            if let Some(group) = item["options"].as_array() {
                collect(group, out);
            } else if item["value"].is_string() {
                out.push(item.clone());
            }
        }
    }
    let mut out = vec![];
    if let Some(items) = option["options"].as_array() {
        collect(items, &mut out);
    }
    out
}

pub fn option<'a>(config: &'a Value, id: &str) -> Option<&'a Value> {
    config["configOptions"]
        .as_array()?
        .iter()
        .find(|o| o["id"] == id)
}
pub fn valid(option: &Value, value: &Value) -> bool {
    match option["type"].as_str() {
        Some("boolean") => value.is_boolean(),
        Some("select") => value.is_string() && choices(option).iter().any(|o| o["value"] == *value),
        _ => false,
    }
}

// Config updates are full replacements. Preserve mode metadata when only options change.
pub fn merge(config: &mut Value, update: &Value) {
    if !config.is_object() {
        *config = json!({});
    }
    for key in ["configOptions", "modes"] {
        if let Some(value) = update.get(key) {
            config[key] = value.clone();
        }
    }
    if let Some(mode) = update["currentModeId"].as_str() {
        if config["modes"].is_object() {
            config["modes"]["currentModeId"] = json!(mode);
        }
        if let Some(options) = config["configOptions"].as_array_mut() {
            for option in options {
                if (option["category"] == "mode" || option["id"] == "mode")
                    && choices(option).iter().any(|o| o["value"] == mode)
                {
                    option["currentValue"] = json!(mode);
                }
            }
        }
    }
}

pub fn publish(s: &Session, update: &Value) {
    let config = {
        let mut data = s.data.lock().unwrap();
        merge(&mut data.session_config, update);
        data.session_config.clone()
    };
    // Synchronize both specialized controls after every response: changing one
    // option may change another option's value or available choices.
    s.set_models(
        agentworks_runtime::provider::parse_model_config(&config, s.provider == "opencode")
            .unwrap_or(Value::Null),
    );
    s.set_permission_modes(crate::permission_modes::parse(&config).unwrap_or(Value::Null));
    s.emit("session_config", config);
}

pub async fn select(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    config: &Value,
    id: &str,
    value: Value,
    legacy_mode: bool,
) -> Result<Value, agent_client_protocol::Error> {
    let future = async {
        if legacy_mode {
            let mode = value
                .as_str()
                .ok_or_else(agent_client_protocol::Error::invalid_params)?;
            if !crate::permission_modes::parse(config).is_some_and(|modes| {
                modes["options"]
                    .as_array()
                    .is_some_and(|options| options.iter().any(|o| o["value"] == mode))
            }) {
                return Err(agent_client_protocol::Error::invalid_params());
            }
            connection
                .send_request(SetSessionModeRequest::new(
                    session_id.to_owned(),
                    mode.to_owned(),
                ))
                .block_task()
                .await?;
            // set_mode has an empty success response; acknowledgement confirms selection.
            Ok(json!({"currentModeId":mode}))
        } else {
            let advertised = option(config, id)
                .filter(|o| valid(o, &value))
                .ok_or_else(agent_client_protocol::Error::invalid_params)?;
            let value = if advertised["type"] == "boolean" {
                SessionConfigOptionValue::boolean(value.as_bool().unwrap())
            } else {
                SessionConfigOptionValue::value_id(value.as_str().unwrap().to_owned())
            };
            let response = connection
                .send_request(SetSessionConfigOptionRequest::new(
                    session_id.to_owned(),
                    id.to_owned(),
                    value,
                ))
                .block_task()
                .await?;
            Ok(serde_json::to_value(response).unwrap_or(Value::Null))
        }
    };
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .map_err(|_| {
            agent_client_protocol::Error::internal_error()
                .data("Session configuration timed out; resume the session to inspect its state")
        })?
}

/// Reapply saved additional settings only when the provider still advertises a
/// compatible value. Each response can change the choices for the next setting.
pub async fn restore(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    mut config: Value,
    saved: &Value,
) -> Result<Value, agent_client_protocol::Error> {
    let mode_id = crate::permission_modes::parse(&config)
        .and_then(|m| m["configId"].as_str().map(str::to_owned));
    if let Some(options) = saved["configOptions"].as_array() {
        for old in options {
            let Some(id) = old["id"].as_str() else {
                continue;
            };
            if old["category"] == "model" || id == "model" || mode_id.as_deref() == Some(id) {
                continue;
            }
            if !matches!(old["type"].as_str(), Some("select" | "boolean")) {
                continue;
            }
            let Some(current) = option(&config, id) else {
                continue;
            };
            if current["currentValue"] == old["currentValue"] {
                continue;
            }
            if !valid(current, &old["currentValue"]) {
                return Err(agent_client_protocol::Error::invalid_params().data(format!("Saved session setting '{id}' is no longer available; inspect session configuration")));
            }
            let selected = select(
                connection,
                session_id,
                &config,
                id,
                old["currentValue"].clone(),
                false,
            )
            .await?;
            merge(&mut config, &selected);
        }
    }
    Ok(config)
}

pub fn legacy_mode(config: &Value) -> bool {
    !config["configOptions"].as_array().is_some_and(|options| {
        options
            .iter()
            .any(|o| o["category"] == "mode" || o["id"] == "mode")
    })
}

pub async fn get(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let config = s.data.lock().unwrap().session_config.clone();
    Ok(Json(config))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Choice {
    config_id: String,
    value: Value,
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
                "Choose session settings while the session is idle",
            ));
        }
        let advertised = option(&data.session_config, &req.config_id)
            .filter(|o| valid(o, &req.value))
            .ok_or_else(|| {
                fail(
                    StatusCode::BAD_REQUEST,
                    "Choose an advertised session setting and value",
                )
            })?;
        // Model changes retain the existing provider policy and validation path.
        if advertised["category"] == "model" || advertised["id"] == "model" {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Use the session model selector",
            ));
        }
        data.status = "configuring".into();
    }
    s.emit("status", json!({"status":"configuring"}));
    if let Err(error) = s.send_command(crate::session_runtime::Command::Config {
        id: req.config_id,
        value: req.value,
    }) {
        s.status("error");
        return Err(error);
    }
    Ok(Json(json!({"accepted":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client};

    fn config() -> Value {
        json!({"configOptions":[
            {"id":"model","name":"Model","category":"model","type":"select","currentValue":"a","options":[{"value":"a","name":"A"}]},
            {"id":"reasoning_effort","name":"Reasoning","category":"thought_level","type":"select","currentValue":"low","options":[{"group":"all","name":"All","options":[{"value":"low","name":"Low"},{"value":"high","name":"High"}]}]},
            {"id":"fast-mode","name":"Fast","type":"boolean","currentValue":false}
        ],"modes":{"currentModeId":"ask","availableModes":[{"id":"ask","name":"Ask"},{"id":"code","name":"Code"}]}})
    }

    #[test]
    fn grouped_choices_and_boolean_values_are_validated_without_coercion() {
        let config = config();
        let effort = option(&config, "reasoning_effort").unwrap();
        assert!(valid(effort, &json!("high")));
        assert!(!valid(effort, &json!("unknown")));
        assert!(!valid(effort, &json!(true)));
        let fast = option(&config, "fast-mode").unwrap();
        assert!(valid(fast, &json!(true)));
        assert!(!valid(fast, &json!("true")));
        assert!(!valid(
            &json!({"type":"future","options":[{"value":"a"}]}),
            &json!("a")
        ));
        assert!(option(&config, "unknown").is_none());
    }

    #[test]
    fn legacy_modes_and_config_updates_replace_and_synchronize_state() {
        let mut config = config();
        assert!(legacy_mode(&config));
        merge(
            &mut config,
            &json!({"sessionUpdate":"current_mode_update","currentModeId":"code"}),
        );
        assert_eq!(
            crate::permission_modes::parse(&config).unwrap()["currentValue"],
            "code"
        );
        merge(
            &mut config,
            &json!({"configOptions":[{"id":"permission","name":"Permission","category":"mode","type":"select","currentValue":"ask","options":[{"value":"ask","name":"Ask"},{"value":"code","name":"Code"}]}]}),
        );
        assert!(!legacy_mode(&config));
        assert!(
            option(&config, "fast-mode").is_none(),
            "updates replace the full option list"
        );
        merge(&mut config, &json!({"currentModeId":"code"}));
        assert_eq!(
            crate::permission_modes::parse(&config).unwrap()["currentValue"],
            "code"
        );
        assert_eq!(
            config["modes"]["availableModes"].as_array().unwrap().len(),
            2
        );
        merge(&mut config, &json!({"configOptions":[]}));
        assert!(legacy_mode(&config));
    }

    // Exercise actual ACP serialization and response handling without a provider
    // installation, account, or model inference.
    #[tokio::test]
    async fn wire_selects_grouped_and_boolean_options_and_legacy_modes() {
        let script = r#"
import sys,json
config=json.loads(sys.argv[1])
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');p=r.get('params',{})
 if m=='initialize':
  assert p['clientCapabilities']['session']['configOptions']['boolean']=={}
  out={'protocolVersion':1,'agentCapabilities':{},'authMethods':[]}
 elif m=='session/set_config_option':
  assert p['sessionId']=='native'
  if p['configId']=='fast-mode':
   assert p['type']=='boolean' and p['value'] is True
  elif p['configId']=='reasoning_effort': assert p['value']=='high'
  else: raise Exception('unexpected setting')
  for o in config['configOptions']:
   if o['id']==p['configId']:o['currentValue']=p['value']
   if o['id']=='model' and p['configId']=='reasoning_effort':o['name']='Updated model'
  out={'configOptions':config['configOptions']}
 elif m=='session/set_mode':
  assert p=={'sessionId':'native','modeId':'code'}
  out={}
 else: raise Exception('unexpected method '+str(m))
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':out}),flush=True)
"#;
        let agent = AcpAgent::new(
            AcpAgentConfig::new("python3")
                .arg("-c")
                .arg(script)
                .arg(config().to_string()),
        );
        Client
            .builder()
            .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
                connection
                    .send_request(agentworks_runtime::acp::initialize_request())
                    .block_task()
                    .await?;
                let mut config = config();
                let invalid = select(
                    &connection,
                    "native",
                    &config,
                    "reasoning_effort",
                    json!("unknown"),
                    false,
                )
                .await;
                assert!(invalid.is_err(), "invalid choices never reach the adapter");
                let selected = select(
                    &connection,
                    "native",
                    &config,
                    "reasoning_effort",
                    json!("high"),
                    false,
                )
                .await?;
                merge(&mut config, &selected);
                assert_eq!(
                    option(&config, "reasoning_effort").unwrap()["currentValue"],
                    "high"
                );
                assert_eq!(option(&config, "model").unwrap()["name"], "Updated model");
                let selected = select(
                    &connection,
                    "native",
                    &config,
                    "fast-mode",
                    json!(true),
                    false,
                )
                .await?;
                merge(&mut config, &selected);
                assert_eq!(option(&config, "fast-mode").unwrap()["currentValue"], true);
                let selected =
                    select(&connection, "native", &config, "mode", json!("code"), true).await?;
                merge(&mut config, &selected);
                assert_eq!(
                    crate::permission_modes::parse(&config).unwrap()["currentValue"],
                    "code"
                );
                let mut reset = config.clone();
                reset["configOptions"][1]["currentValue"] = json!("low");
                reset["configOptions"][2]["currentValue"] = json!(false);
                let restored = restore(&connection, "native", reset, &config).await?;
                assert_eq!(
                    option(&restored, "reasoning_effort").unwrap()["currentValue"],
                    "high"
                );
                assert_eq!(
                    option(&restored, "fast-mode").unwrap()["currentValue"],
                    true
                );
                let mut unavailable = config.clone();
                unavailable["configOptions"][1]["currentValue"] = json!("removed");
                assert!(
                    restore(&connection, "native", config, &unavailable)
                        .await
                        .is_err()
                );
                Ok::<_, agent_client_protocol::Error>(())
            })
            .await
            .unwrap();
    }
}
