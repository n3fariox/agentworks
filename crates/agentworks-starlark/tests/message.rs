use agentworks_starlark::INBOX_MESSAGE_SCRIPT;
use agentworks_starlark::engine::{Capability, DiagnosticKind, Engine, Installation};
use serde_json::json;

#[test]
fn formatter_preserves_markdown_and_marks_truncation() {
    let engine = Engine::new().unwrap();
    let script = engine
        .compile(
            &Installation::of("inbox_message.star", Capability::MessageFormatter, INBOX_MESSAGE_SCRIPT),
            INBOX_MESSAGE_SCRIPT,
        )
        .unwrap();
    let text = "Please review **UI Hover**.\n\n- Branch: `main`\n\n```diff\n+ change\n```";
    for truncated in [false, true] {
        let (value, _) = engine
            .call(
                &script,
                &json!({"channel":"inbox","state":{}}),
                &json!({
                    "id":187,"sender":"peer","senderTitle":"UI Hover","text":text,"truncated":truncated,
                }),
            )
            .unwrap();
        let result = value["result"].as_str().unwrap();
        assert!(result.starts_with(text));
        assert_eq!(result.contains("Message truncated"), truncated);
    }
}

#[test]
fn formatter_is_typed_bounded_and_has_no_action_capability() {
    let engine = Engine::new().unwrap();
    for source in [
        INBOX_MESSAGE_SCRIPT.replace("return text", "return 123"),
        INBOX_MESSAGE_SCRIPT.replace("message.text", "message.missing"),
        INBOX_MESSAGE_SCRIPT.replace("message.text", "regex_match(\"x\", \"x\")"),
        INBOX_MESSAGE_SCRIPT.replace("@agentworks//message.star", "@agentworks//workflow.star"),
    ] {
        assert!(
            engine
                .compile(
                    &Installation::of("test.star", Capability::MessageFormatter, &source),
                    &source
                )
                .is_err()
        );
    }
    let source = INBOX_MESSAGE_SCRIPT.replace("return text", "return text * 10000");
    let script = engine
        .compile(
            &Installation::of("large.star", Capability::MessageFormatter, &source),
            &source,
        )
        .unwrap();
    let err = engine
        .call(
            &script,
            &json!({"channel":"inbox","state":{}}),
            &json!({
                "id":1,"sender":"peer","senderTitle":"Peer","text":"0123456789","truncated":false,
            }),
        )
        .unwrap_err();
    assert_eq!(err.kind, DiagnosticKind::Limit);
}
