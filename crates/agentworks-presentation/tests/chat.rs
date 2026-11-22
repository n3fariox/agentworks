use agentworks_presentation::chat::{LineFormatter, apply_in_session, format_in_session};
use std::cell::RefCell;

#[derive(Default)]
struct Formatter(RefCell<Vec<String>>);
impl LineFormatter for Formatter {
    fn format_chat_in_session(
        &self,
        session: Option<&str>,
        kind: &str,
        provider: &str,
        cwd: &str,
        lines: Vec<String>,
    ) -> Option<Vec<Option<String>>> {
        assert_eq!(
            (session, kind, provider, cwd),
            (Some("s"), "chat.agent", "demo", "/w")
        );
        *self.0.borrow_mut() = lines.clone();
        Some(
            lines
                .into_iter()
                .map(|line| Some(format!("**{line}**")))
                .collect(),
        )
    }
}

#[test]
fn formatter_receives_only_prose_and_output_keeps_crlf_and_code() {
    let formatter = Formatter::default();
    let text = "before\r\n```rust\r\nlet x = 1;\r\n```\r\nafter\r";
    let formatted = apply_in_session(&formatter, "s", "demo", "/w", text);
    assert_eq!(*formatter.0.borrow(), ["before", "after"]);
    assert_eq!(
        formatted,
        "**before**\r\n```rust\r\nlet x = 1;\r\n```\r\n**after**\r"
    );
}

#[test]
fn prose_limit_preserves_the_remaining_lines() {
    let formatter = Formatter::default();
    let text = vec!["line"; 2002].join("\n");
    let formatted =
        format_in_session(&formatter, Some("s"), "chat.agent", "demo", "/w", &text).unwrap();
    assert_eq!(formatter.0.borrow().len(), 2000);
    assert_eq!(formatted.len(), 2002);
    assert!(formatted[..2000].iter().all(Option::is_some));
    assert_eq!(&formatted[2000..], [None, None]);
}
