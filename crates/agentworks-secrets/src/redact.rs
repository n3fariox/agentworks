//! Replacing delivered secret values in text an agent or the browser can see.
//! Literal matching only: a deliberately transformed value is not caught.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
};
use serde_json::Value;
use std::{collections::HashMap, sync::Mutex};
use zeroize::Zeroizing;

/// Each encoded form of a value with the label that replaces it, longest first.
pub type Forms = Vec<(Zeroizing<String>, String)>;

/// Values this short are too likely to occur by chance to replace.
const MIN_BYTES: usize = 4;

/// The raw value and the encodings it most often appears in: unpadded base64 and
/// base64url, percent-encoding, and a JSON string body.
fn forms(name: &str, value: &str) -> Forms {
    if value.len() < MIN_BYTES {
        return Vec::new();
    }
    let percent: String = value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    let json = serde_json::to_string(value).unwrap();
    let mut out = vec![
        value.to_owned(),
        STANDARD_NO_PAD.encode(value),
        URL_SAFE_NO_PAD.encode(value),
        percent,
        json[1..json.len() - 1].to_owned(),
    ];
    out.sort();
    out.dedup();
    let label = format!("[redacted:{name}]");
    out.into_iter()
        .map(|form| (Zeroizing::new(form), label.clone()))
        .collect()
}
fn sort(forms: &mut Forms) {
    // Longest first, so a value is never partly replaced by a shorter one it contains.
    forms.sort_by_key(|(form, _)| std::cmp::Reverse(form.len()));
}
/// The forms of a set of `(name, value)` pairs.
pub fn forms_for<'a>(vars: impl Iterator<Item = (&'a str, &'a str)>) -> Forms {
    let mut all: Forms = vars.flat_map(|(name, value)| forms(name, value)).collect();
    sort(&mut all);
    all
}
pub fn replace(forms: &Forms, text: &str) -> String {
    let mut out = text.to_owned();
    for (form, label) in forms {
        if out.contains(form.as_str()) {
            out = out.replace(form.as_str(), label);
        }
    }
    out
}
/// Replaces the values in every string of `value`.
pub fn replace_json(forms: &Forms, value: &mut Value) {
    match value {
        Value::String(s) => *s = replace(forms, s),
        Value::Array(a) => a.iter_mut().for_each(|v| replace_json(forms, v)),
        Value::Object(o) => o.values_mut().for_each(|v| replace_json(forms, v)),
        _ => {}
    }
}

/// Per-session forms of every value delivered to that session.
#[derive(Default)]
pub struct Scrubbers(Mutex<HashMap<String, Forms>>);
impl Scrubbers {
    /// Adds values delivered to a session. Earlier values stay registered, since they
    /// may still appear in later output.
    pub fn register(&self, session: &str, vars: &[(String, Zeroizing<String>)]) {
        if vars.is_empty() {
            return;
        }
        let mut scrubbers = self.0.lock().unwrap();
        let all = scrubbers.entry(session.to_owned()).or_default();
        for form in vars.iter().flat_map(|(name, value)| forms(name, value)) {
            if !all.iter().any(|(known, _)| **known == *form.0) {
                all.push(form);
            }
        }
        sort(all);
    }
    pub fn forget(&self, session: &str) {
        self.0.lock().unwrap().remove(session);
    }
    pub fn text(&self, session: &str, text: &str) -> String {
        match self.0.lock().unwrap().get(session) {
            Some(forms) => replace(forms, text),
            None => text.to_owned(),
        }
    }
    pub fn json(&self, session: &str, value: &mut Value) {
        if let Some(forms) = self.0.lock().unwrap().get(session) {
            replace_json(forms, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::json;

    #[test]
    fn delivered_values_and_their_encodings_are_scrubbed_per_session() {
        let scrubbers = Scrubbers::default();
        let value = "tok/en+value=1&x";
        scrubbers.register(
            "a",
            &[
                ("TOKEN".into(), Zeroizing::new(value.into())),
                ("SHORT".into(), Zeroizing::new("abc".into())),
            ],
        );
        let label = "[redacted:TOKEN]";
        let mut event = json!({"text": format!("a {value} b"), "nested": [{
            "b64": STANDARD.encode(value), "url": URL_SAFE_NO_PAD.encode(value),
            "pct": "tok%2Fen%2Bvalue%3D1%26x", "json": serde_json::to_string(&json!({"v": value})).unwrap()}],
            "short": "abc", "n": 1});
        scrubbers.json("a", &mut event);
        assert_eq!(event["text"], format!("a {label} b"));
        let nested = &event["nested"][0];
        assert!(nested["b64"].as_str().unwrap().starts_with(label));
        assert_eq!(nested["url"], label);
        assert_eq!(nested["pct"], label);
        assert!(nested["json"].as_str().unwrap().contains(label));
        assert_eq!(event["short"], "abc", "short values are not replaced");
        assert!(!event.to_string().contains(value));
        assert_eq!(
            scrubbers.text("b", value),
            value,
            "other sessions are untouched"
        );
        // A longer value containing a shorter one is replaced whole.
        scrubbers.register(
            "c",
            &[
                ("INNER".into(), Zeroizing::new("secret".into())),
                ("OUTER".into(), Zeroizing::new("my-secret-value".into())),
            ],
        );
        assert_eq!(
            scrubbers.text("c", "my-secret-value secret"),
            "[redacted:OUTER] [redacted:INNER]"
        );
        scrubbers.forget("a");
        assert_eq!(scrubbers.text("a", value), value);
    }
}
