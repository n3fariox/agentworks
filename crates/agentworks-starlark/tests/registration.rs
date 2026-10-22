use agentworks_starlark::engine::{Capability, DiagnosticKind, Engine};
use serde_json::json;
use std::sync::Arc;

const FUNCTIONS: &str = r#"
load("@agentworks//progress.star", "LineContext", "Progress")
load("@agentworks//message.star", "MessageContext", "PeerMessage")
def collect(ctx: LineContext, line: str) -> Progress | None:
    ctx.state["last"] = line
    return Progress(phase = "Building", label = None, counts = [])
def pretty(ctx: MessageContext, message: PeerMessage) -> str:
    return "**" + message.text + "**"
"#;
const REGISTRATIONS: &str = r#"
agent.register_progress(collect, command_pattern = "^make\\b", display_name = "Make")
agent.register_formatter(pretty, message_type = "inbox.peer", text_pattern = "^Review")
"#;

#[test]
fn one_module_registers_both_hooks_with_arbitrary_function_and_file_names() {
    let engine = Engine::new().unwrap();
    let source = format!("{FUNCTIONS}{REGISTRATIONS}");
    let installed = engine.compile_registered("my_handlers.star", &source).unwrap();
    assert_eq!(installed.hooks.len(), 2);
    assert_eq!(installed.hooks[0].hook, "progress");
    assert_eq!(installed.hooks[1].message_type.as_deref(), Some("inbox.peer"));
    let parser = &installed.handlers[0];
    assert_eq!(parser.capability, Capability::Parser);
    assert!(parser.command_pattern.as_ref().unwrap().is_match("make build"));
    let (parsed, _) = engine
        .call(
            parser,
            &json!({"command":"make build","stream":"t","provisional":false,"state":{}}),
            &json!("a line"),
        )
        .unwrap();
    assert_eq!(parsed["result"]["phase"], "Building");
    assert_eq!(parsed["state"]["last"], "a line");
    let formatter = &installed.handlers[1];
    assert!(formatter.text_pattern.as_ref().unwrap().is_match("Review this"));
    assert!(!formatter.text_pattern.as_ref().unwrap().is_match("Other message"));
    let (formatted, _) = engine
        .call(
            formatter,
            &json!({"channel":"inbox","state":{}}),
            &json!({"id":1,"sender":"p","senderTitle":"Peer","text":"Review this"}),
        )
        .unwrap();
    assert_eq!(formatted["result"], "**Review this**");
    assert!(Arc::ptr_eq(
        &installed,
        &engine.compile_registered("my_handlers.star", &source).unwrap()
    ));
    assert!(!Arc::ptr_eq(
        &installed,
        &engine.compile_registered("other.star", &source).unwrap()
    ));
}

#[test]
fn declarations_are_required_and_all_are_validated_atomically() {
    let engine = Engine::new().unwrap();
    assert_eq!(
        engine.compile_registered("none.star", FUNCTIONS).err().unwrap().kind,
        DiagnosticKind::Install
    );
    for declarations in [
        "agent.register_progress(pretty, command_pattern = \"x\")",
        "agent.register_formatter(collect, message_type = \"inbox.peer\")",
        "agent.register_formatter(123, message_type = \"inbox.peer\")",
        "agent.register_formatter(pretty, message_type = \"unknown\")",
        "agent.register_formatter(pretty, message_type = \"inbox.peer\", text_pattern = \"[\")",
        "agent.register_progress(collect, command_pattern = \"[\")",
        "agent.register_progress(collect, command_pattern = \"x\", display_name = \"\")",
    ] {
        let source = format!("{FUNCTIONS}{REGISTRATIONS}\n{declarations}\n");
        let err = engine
            .compile_registered("bad.star", &source)
            .err()
            .expect("one invalid hook rejects the whole installation");
        assert_eq!(err.kind, DiagnosticKind::Type, "{err}");
    }
    let too_many = format!(
        "{FUNCTIONS}{}",
        "agent.register_formatter(pretty, message_type = \"inbox.peer\")\n".repeat(33)
    );
    assert!(engine.compile_registered("many.star", &too_many).is_err());
}

#[test]
fn hook_registrations_cannot_change_during_event_handling() {
    let engine = Engine::new().unwrap();
    let source = format!(
        "{FUNCTIONS}\ndef runtime(ctx: MessageContext, message: PeerMessage) -> str:\n    agent.register_formatter(pretty, message_type = \"inbox.peer\")\n    return message.text\nagent.register_formatter(runtime, message_type = \"inbox.peer\")\n"
    );
    let script = engine.compile_registered("runtime.star", &source).unwrap();
    let err = engine
        .call(
            &script.handlers[0],
            &json!({"channel":"inbox","state":{}}),
            &json!({"id":1,"sender":"p","senderTitle":"Peer","text":"hello"}),
        )
        .unwrap_err();
    assert_eq!(err.kind, DiagnosticKind::Runtime);
    assert!(err.message.contains("only be registered during script installation"));
}

#[test]
fn registration_cannot_load_actions_and_still_checks_uncalled_functions() {
    let engine = Engine::new().unwrap();
    let source = format!("load(\"@agentworks//workflow.star\", \"start_session\")\n{FUNCTIONS}{REGISTRATIONS}");
    assert_eq!(
        engine.compile_registered("actions.star", &source).err().unwrap().kind,
        DiagnosticKind::Capability
    );
    let source = format!("{FUNCTIONS}{REGISTRATIONS}\ndef unused() -> str:\n    return 123\n");
    assert_eq!(
        engine.compile_registered("typed.star", &source).err().unwrap().kind,
        DiagnosticKind::Type
    );
}
