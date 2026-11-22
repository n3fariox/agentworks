//! Native Antigravity extensions. Quota is a CLI command, never an ACP prompt.
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

pub fn supports_system_prompt(init: &Value) -> bool {
    init["agentCapabilities"]["_meta"]["antigravity"]["systemPrompt"]["append"] == true
}

pub fn quota_report(raw: &Value) -> Result<Value, String> {
    if raw["status"] != "SUCCESS"
        || raw["command"]["name"] != "usage"
        || raw["num_turns"].as_u64() != Some(0)
    {
        return Err(
            "Antigravity did not return a native quota report; update agy and retry".into(),
        );
    }
    let groups = raw["command"]["data"]["groups"]
        .as_array()
        .filter(|g| g.len() <= 64)
        .ok_or("Invalid Antigravity quota groups")?;
    let mut windows = Vec::new();
    for group in groups {
        let Some(name) = group["name"].as_str().filter(|s| s.len() <= 200) else {
            continue;
        };
        for bucket in group["buckets"].as_array().into_iter().flatten().take(64) {
            let Some(id) = bucket["id"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 200)
            else {
                continue;
            };
            let fraction = bucket["remaining_fraction"]
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v));
            let reset = bucket["reset_time"].as_str().filter(|s| s.len() <= 128);
            windows.push(json!({"id":id,"group":name,"name":bucket["name"].as_str().filter(|s| s.len() <= 200),
                "window":bucket["window"].as_str().filter(|s| s.len() <= 40),"remainingFraction":fraction,"resetTime":reset}));
        }
    }
    Ok(json!({"windows":windows}))
}

pub async fn quota(environment: BTreeMap<String, String>) -> Result<Value, String> {
    let executable = environment
        .get("AGENTWORKS_ANTIGRAVITY_PATH")
        .map(PathBuf::from)
        .or_else(crate::provider::antigravity_path)
        .unwrap_or_else(|| "agy".into());
    let home = environment
        .get("HOME")
        .or_else(|| environment.get("USERPROFILE"))
        .cloned()
        .or_else(|| std::env::var("HOME").ok())
        .or_else(|| std::env::var("USERPROFILE").ok());
    let mut command = Command::new(executable);
    command
        .args(["--print", "/usage", "--output-format", "json"])
        .envs(environment)
        .env("GEMINI_API_KEY", "")
        .env("GOOGLE_API_KEY", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(home) = home {
        command.current_dir(home);
    }
    for name in crate::provider::SERVICE_ENVIRONMENT
        .iter()
        .copied()
        .chain(["AGENTWORKS_SESSION_ID", "AGENTWORKS_SESSION_URI"])
    {
        command.env_remove(name);
    }
    let mut child = command.spawn().map_err(
        |_| "Could not start Antigravity quota command; install agy and sign in on this host",
    )?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Could not read Antigravity quota report")?;
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let mut bytes = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Could not read Antigravity quota report")?;
        if bytes.len() > 1024 * 1024 {
            return Err("Antigravity quota report exceeded its size limit");
        }
        if !child
            .wait()
            .await
            .map_err(|_| "Antigravity quota command failed")?
            .success()
        {
            return Err("Antigravity quota command failed; check agy sign-in on this host");
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| "Antigravity quota command timed out")??;
    let raw = serde_json::from_slice(&result)
        .map_err(|_| "Antigravity returned an invalid quota report")?;
    quota_report(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_only_accepts_native_commands_and_bounds_values() {
        let mut raw = json!({"status":"SUCCESS","num_turns":0,"command":{"name":"usage","data":{"groups":[{"name":"Gemini","buckets":[
            {"id":"5h","remaining_fraction":0.75,"reset_time":"2026-11-11T19:00:00Z"},
            {"id":"weekly","remaining_fraction":1.5}]}]}}});
        let report = quota_report(&raw).unwrap();
        assert_eq!(report["windows"][0]["remainingFraction"], 0.75);
        assert!(report["windows"][1]["remainingFraction"].is_null());
        raw["num_turns"] = json!(1);
        assert!(quota_report(&raw).is_err());
        raw["num_turns"] = json!(0);
        raw["command"]["name"] = json!("prompt");
        assert!(quota_report(&raw).is_err());
    }
}
