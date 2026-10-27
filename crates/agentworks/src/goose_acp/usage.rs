//! Codex response headers use the same quota metadata as our Codex ACP bridges.
//! Values retain their own observation times across partial responses and replay.
use reqwest::header::HeaderMap;
use serde_json::{Map, Value, json};

#[derive(Default)]
pub(super) struct Quota {
    limits: Map<String, Value>,
}

impl Quota {
    pub(super) fn observe(&mut self, headers: &HeaderMap) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.observe_at(headers, timestamp);
    }

    fn observe_at(&mut self, headers: &HeaderMap, timestamp: u64) {
        // Discover additional model-specific families as well as the default.
        // https://github.com/openai/codex/blob/main/codex-rs/codex-api/src/rate_limits.rs
        let mut prefixes = std::collections::BTreeSet::from(["x-codex".to_owned()]);
        for name in headers.keys() {
            for role in ["primary", "secondary"] {
                for field in ["used-percent", "window-minutes", "reset-at"] {
                    if let Some(prefix) = name.as_str().strip_suffix(&format!("-{role}-{field}"))
                        && prefix.starts_with("x-")
                    {
                        prefixes.insert(prefix.to_owned());
                    }
                }
            }
        }
        for prefix in prefixes {
            let id = prefix.trim_start_matches("x-").replace('-', "_");
            for role in ["primary", "secondary"] {
                for (suffix, field) in [
                    ("used-percent", "usedPercent"),
                    ("window-minutes", "windowDurationMins"),
                    ("reset-at", "resetsAt"),
                ] {
                    let Some(raw) = headers.get(format!("{prefix}-{role}-{suffix}")) else {
                        continue;
                    };
                    let value = raw
                        .to_str()
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|n| n.is_finite() && *n >= 0.0)
                        .filter(|n| match field {
                            "usedPercent" => *n <= 100.0,
                            "windowDurationMins" => *n > 0.0 && n.fract() == 0.0,
                            _ => n.fract() == 0.0 && *n <= 8.64e12,
                        });
                    let entry = self
                        .limits
                        .entry(&id)
                        .or_insert_with(|| json!({"limitId":id}));
                    if let Some(name) = headers
                        .get(format!("{prefix}-limit-name"))
                        .and_then(|v| v.to_str().ok())
                    {
                        entry["limitName"] = json!(name);
                    }
                    if !entry[role].is_object() {
                        entry[role] = json!({});
                    }
                    // Invalid reports clear that field; absent headers preserve it.
                    entry[role][field] = json!({"value":value,"observedAtMs":timestamp});
                }
            }
        }
    }

    pub(super) fn attach(&self, update: &mut Value) {
        if self.limits.is_empty() || !update["parentSessionId"].is_null() {
            return;
        }
        if !update["_meta"].is_object() {
            update["_meta"] = json!({});
        }
        update["_meta"]["_codex/subscriptionLimits"] =
            json!({"version":1,"limits":self.limits.values().collect::<Vec<_>>()});
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_headers_preserve_times_and_invalid_fields_clear() {
        let mut quota = Quota::default();
        let headers: HeaderMap = [
            ("x-codex-primary-used-percent", "12.5"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-at", "1800000000"),
            ("x-codex-secondary-used-percent", "25"),
            ("x-codex-secondary-window-minutes", "10080"),
            ("x-codex-fast-primary-used-percent", "80"),
            ("x-codex-fast-limit-name", "Fast model"),
        ]
        .into_iter()
        .map(|(k, v)| {
            (
                k.parse::<reqwest::header::HeaderName>().unwrap(),
                v.parse().unwrap(),
            )
        })
        .collect();
        quota.observe_at(&headers, 1000);
        let later: HeaderMap = [
            ("x-codex-primary-used-percent", "20"),
            ("x-codex-secondary-used-percent", "NaN"),
            ("x-codex-primary-reset-at", "-1"),
        ]
        .into_iter()
        .map(|(k, v)| {
            (
                k.parse::<reqwest::header::HeaderName>().unwrap(),
                v.parse().unwrap(),
            )
        })
        .collect();
        quota.observe_at(&later, 2000);
        quota.observe_at(&HeaderMap::new(), 3000);
        assert_eq!(
            quota.limits["codex"]["primary"]["usedPercent"],
            json!({"value":20.0,"observedAtMs":2000})
        );
        assert_eq!(
            quota.limits["codex"]["primary"]["windowDurationMins"],
            json!({"value":300.0,"observedAtMs":1000})
        );
        assert_eq!(
            quota.limits["codex"]["secondary"]["usedPercent"]["value"],
            Value::Null
        );
        assert_eq!(
            quota.limits["codex"]["primary"]["resetsAt"]["value"],
            Value::Null
        );
        assert_eq!(quota.limits["codex_fast"]["limitName"], "Fast model");
        let mut update = json!({"used":7,"size":200000,"_meta":{"other":true}});
        quota.attach(&mut update);
        assert_eq!(update["used"], 7);
        assert_eq!(update["_meta"]["other"], true);
        assert_eq!(
            update["_meta"]["_codex/subscriptionLimits"]["limits"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let mut child = json!({"parentSessionId":"parent"});
        quota.attach(&mut child);
        assert!(child.get("_meta").is_none());
    }

    #[test]
    fn no_headers_means_no_quota() {
        let mut quota = Quota::default();
        quota.observe_at(&HeaderMap::new(), 1000);
        let mut update = json!({"used":7,"size":200000});
        quota.attach(&mut update);
        assert!(update.get("_meta").is_none());
    }

    #[test]
    fn rejects_out_of_range_fields_and_reads_partial_model_headers() {
        let mut quota = Quota::default();
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-primary-used-percent", "101".parse().unwrap());
        headers.insert("x-codex-primary-window-minutes", "0".parse().unwrap());
        headers.insert("x-codex-primary-reset-at", "inf".parse().unwrap());
        headers.insert(
            "x-codex-fast-secondary-reset-at",
            "1800000000".parse().unwrap(),
        );
        quota.observe_at(&headers, 1000);
        for field in ["usedPercent", "windowDurationMins", "resetsAt"] {
            assert_eq!(
                quota.limits["codex"]["primary"][field]["value"],
                Value::Null
            );
        }
        assert_eq!(
            quota.limits["codex_fast"]["secondary"]["resetsAt"]["value"],
            json!(1800000000.0)
        );
        assert!(
            quota.limits["codex_fast"]["secondary"]
                .get("usedPercent")
                .is_none()
        );
    }
}
