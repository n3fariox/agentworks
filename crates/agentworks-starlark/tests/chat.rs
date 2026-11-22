use agentworks_starlark::engine::{Capability, DiagnosticKind, Engine, prose_segments};
use serde_json::json;

const JIRA: &str = r#"
load("@agentworks//chat.star", "ChatContext")

def link_tickets(ctx: ChatContext, line: str) -> str | None:
    return map_prose(line, lambda text: regex_replace("\\b([A-Z][A-Z0-9]+-[0-9]+)\\b", text, "[$1](https://jira.example/browse/$1)"))

agent.register_formatter(link_tickets, message_type = "chat.agent", text_pattern = "\\b[A-Z][A-Z0-9]+-[0-9]+\\b")
"#;

fn format(engine: &Engine, source: &str, line: &str) -> serde_json::Value {
    let installed = engine.compile_registered("chat.star", source).unwrap();
    let script = &installed.handlers[0];
    assert_eq!(script.capability, Capability::ChatFormatter);
    assert_eq!(installed.hooks[0].message_type.as_deref(), Some("chat.agent"));
    let (value, _) = engine
        .call(
            script,
            &json!({"provider":"claude","cwd":"/work","state":{}}),
            &json!(line),
        )
        .unwrap();
    value["result"].clone()
}

#[test]
fn chat_formatters_rewrite_prose_and_leave_code_and_links_alone() {
    let engine = Engine::new().unwrap();
    assert_eq!(
        format(
            &engine,
            JIRA,
            "Fixed OPS-12 and `OPS-13`; see [OPS-14](https://x.example/OPS-14) or https://jira.example/browse/OPS-15, then ``a ` OPS-16``."
        ),
        "Fixed [OPS-12](https://jira.example/browse/OPS-12) and `OPS-13`; see [OPS-14](https://x.example/OPS-14) or https://jira.example/browse/OPS-15, then ``a ` OPS-16``."
    );
    // An unmatched backtick is prose, as in CommonMark.
    assert_eq!(
        format(&engine, JIRA, "a ` OPS-1"),
        "a ` [OPS-1](https://jira.example/browse/OPS-1)"
    );
}

#[test]
fn chat_formatter_context_and_none_results() {
    let engine = Engine::new().unwrap();
    let source = r#"
load("@agentworks//chat.star", "ChatContext")
def tag(ctx: ChatContext, line: str) -> str | None:
    if line.startswith("skip"):
        return None
    return ctx.provider + ":" + ctx.cwd + ":" + line
agent.register_formatter(tag, message_type = "chat.agent")
"#;
    assert_eq!(format(&engine, source, "hello"), "claude:/work:hello");
    assert_eq!(format(&engine, source, "skip me"), json!(null));
}

#[test]
fn chat_formatters_must_take_a_line() {
    let engine = Engine::new().unwrap();
    let wrong = r#"
load("@agentworks//message.star", "MessageContext", "PeerMessage")
def pretty(ctx: MessageContext, message: PeerMessage) -> str:
    return message.text
agent.register_formatter(pretty, message_type = "chat.agent")
"#;
    let error = engine.compile_registered("wrong.star", wrong).err().unwrap();
    assert_eq!(error.kind, DiagnosticKind::Type);
    assert!(
        error.message.contains("def(ctx: ChatContext, line: str) -> None | str"),
        "{}",
        error.message
    );
    let unknown = JIRA.replace("chat.agent", "chat.nobody");
    let error = engine.compile_registered("unknown.star", &unknown).err().unwrap();
    assert!(
        error.message.contains("supported: inbox.peer, chat.agent"),
        "{}",
        error.message
    );
}

#[test]
fn prose_segments_cover_the_line_exactly() {
    for line in [
        "",
        "plain",
        "`code`",
        "a <b> c",
        "x ![i](p.png) y",
        "``` unclosed",
        "é `ü` ö https://a.b/c d",
    ] {
        let segments = prose_segments(line);
        assert_eq!(segments.iter().map(|(_, s)| *s).collect::<String>(), line);
    }
    assert_eq!(
        prose_segments("a `b` <c> d"),
        vec![(true, "a "), (false, "`b`"), (true, " "), (false, "<c>"), (true, " d")]
    );
}

#[test]
fn formatters_without_text_pattern_run_no_regex_and_chat_user_is_a_chat_formatter() {
    let engine = Engine::new().unwrap();
    for registration in [
        r#"agent.register_formatter(format_line, message_type = "chat.user")"#,
        r#"agent.register_formatter(format_line, message_type = "chat.user", text_pattern = None)"#,
    ] {
        let source = format!(
            "load(\"@agentworks//chat.star\", \"ChatContext\")\ndef format_line(ctx: ChatContext, line: str) -> str | None:\n    return None\n{registration}\n"
        );
        let installed = engine.compile_registered("user.star", &source).unwrap();
        assert_eq!(installed.handlers[0].capability, Capability::ChatFormatter);
        assert_eq!(installed.handlers[0].message_type.as_deref(), Some("chat.user"));
        assert!(installed.handlers[0].text_pattern.is_none(), "{registration}");
        assert!(installed.hooks[0].pattern.is_none());
        assert!(
            serde_json::to_value(&installed.hooks[0])
                .unwrap()
                .get("pattern")
                .is_none()
        );
    }
    let inbox = engine
        .compile_registered("inbox_message.star", agentworks_starlark::INBOX_MESSAGE_SCRIPT)
        .unwrap();
    assert!(
        inbox.handlers[0].text_pattern.is_none(),
        "the built-in inbox formatter sees every message"
    );
}
