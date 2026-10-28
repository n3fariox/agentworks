//! Claude's and Codex's native `/rename` also names the Agentworks session. Harnesses report
//! generated titles through the same `session_info_update`, so a reported title is
//! adopted only while a `/rename` turn runs; any other title stays the harness's own.
use crate::Session;
use serde_json::Value;

const TITLE_LIMIT: usize = 80;

/// Whether a prompt is a native `/rename`, and the title it names. A bare
/// `/rename` asks Claude to choose one, which arrives later as an update; Codex
/// requires a name and refuses a bare `/rename`, so that is not a rename.
pub fn parse(provider: &str, text: &str) -> Option<Option<String>> {
    let codex = provider.starts_with("codex");
    if !provider.starts_with("claude") && !codex {
        return None;
    }
    let rest = text.trim().strip_prefix("/rename")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let title = clean(rest);
    if codex && title.is_none() {
        return None;
    }
    Some(title)
}

/// One line within the Agentworks title limit, or None when nothing remains.
pub fn clean(raw: &str) -> Option<String> {
    let title = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    if title.is_empty() {
        return None;
    }
    Some(if title.chars().count() > TITLE_LIMIT {
        title.chars().take(TITLE_LIMIT - 1).chain(['…']).collect()
    } else {
        title
    })
}

/// Adopts the title a harness reports for the running `/rename` turn.
pub fn observe(s: &Session, update: &Value) {
    if update["sessionUpdate"] != "session_info_update" {
        return;
    }
    let Some(title) = update["title"].as_str().and_then(clean) else {
        return;
    };
    {
        let data = s.data.lock().unwrap();
        if !data.native_rename || !data.turn_active {
            return;
        }
    }
    adopt(s, title);
}

pub fn adopt(s: &Session, title: String) {
    if s.data.lock().unwrap().lifecycle.title.as_deref() == Some(title.as_str()) {
        return;
    }
    match crate::lifecycle::commit(s, Some(title), None) {
        Ok(()) => s.emit("session_updated", s.meta()),
        Err((_, error)) => eprintln!("Could not adopt the native session title: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_native_rename_prompts() {
        assert_eq!(
            parse("claude", "/rename  Parser\twork "),
            Some(Some("Parser work".into()))
        );
        assert_eq!(parse("claude-rust", "/rename"), Some(None));
        assert_eq!(parse("claude", "/renamed thing"), None);
        assert_eq!(parse("claude", "please /rename this"), None);
        assert_eq!(
            parse("codex", " /rename  Parser work"),
            Some(Some("Parser work".into()))
        );
        assert_eq!(
            parse("codex-rust", "/rename Parser"),
            Some(Some("Parser".into()))
        );
        assert_eq!(
            parse("codex", "/rename"),
            None,
            "Codex refuses a bare /rename"
        );
        assert_eq!(parse("goose", "/rename Parser"), None);
        assert_eq!(parse("opencode", "/rename Parser"), None);
    }

    #[test]
    fn cleans_titles_to_the_session_limit() {
        assert_eq!(clean(" \n "), None);
        assert_eq!(clean("a\u{7}b"), Some("ab".into()));
        let long = clean(&"x".repeat(200)).unwrap();
        assert_eq!(long.chars().count(), TITLE_LIMIT);
        assert!(long.ends_with('…'));
    }
}
