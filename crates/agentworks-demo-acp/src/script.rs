//! README media fixture prompts, kept in file order so the first matching `^` prefix wins.
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;

#[derive(Default)]
pub struct Script(Vec<(String, Value)>);

impl Script {
    /// Exact prompts first, then keys starting with `^` as prefixes.
    pub fn find(&self, text: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(key, _)| key == text)
            .or_else(|| {
                self.0.iter().find(|(key, _)| {
                    key.strip_prefix('^')
                        .is_some_and(|prefix| text.starts_with(prefix))
                })
            })
            .map(|(_, steps)| steps)
            .filter(|steps| !matches!(steps, Value::Null | Value::Bool(false)))
    }
}

impl<'de> Deserialize<'de> for Script {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Ordered;
        impl<'de> Visitor<'de> for Ordered {
            type Value = Script;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object mapping prompts to steps")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Script, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Script(entries))
            }
        }
        deserializer.deserialize_map(Ordered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_keys_win_then_prefixes_in_file_order() {
        let script: Script =
            serde_json::from_str(r#"{"^b":[2],"^a":[1],"abc":[0],"^ab":[3]}"#).unwrap();
        assert_eq!(script.find("abc"), Some(&serde_json::json!([0])));
        assert_eq!(script.find("abd"), Some(&serde_json::json!([1])));
        assert_eq!(script.find("zzz"), None);
    }
}
