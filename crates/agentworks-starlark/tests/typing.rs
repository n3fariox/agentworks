//! Type hints are enabled and enforced at install (static) and call (runtime) time.
mod common;
use agentworks_starlark::engine::{Capability, DiagnosticKind, Engine, Installation, dialect};
use common::*;
use serde_json::json;
use starlark::syntax::DialectTypes;

fn install_error(capability: Capability, source: &str) -> agentworks_starlark::engine::Diagnostic {
    match install(&engine(), "typed.star", capability, source) {
        Ok(_) => panic!("expected install failure for:\n{source}"),
        Err(d) => d,
    }
}

#[test]
fn dialect_enables_runtime_type_annotations() {
    assert_eq!(dialect().enable_types, DialectTypes::Enable);
}

#[test]
fn typed_samples_install_with_exact_host_signatures() {
    let engine = engine();
    let parser = install(&engine, "bazel_progress.star", Capability::Parser, BAZEL).unwrap();
    let workflow = install(&engine, "check_then_fix.star", Capability::Workflow, WORKFLOW).unwrap();
    assert_eq!(parser.capability, Capability::Parser);
    assert_eq!(workflow.capability, Capability::Workflow);
    assert_eq!(
        Capability::Parser.signature(),
        "def(ctx: LineContext, line: str) -> None | Progress"
    );
    assert_eq!(
        Capability::Workflow.signature(),
        "def(ctx: WorkflowContext, event: Event) -> list[Action]"
    );
}

#[test]
fn static_errors_are_rejected_at_install_even_in_uncalled_code() {
    let cases = [
        (
            "    return ctx.stat\n",
            "attribute `stat` is not available on the type `LineContext`",
        ),
        (
            "    return ctx\n",
            "Expected type `None | Progress` but got `LineContext`",
        ),
        (
            "    return Progress(phase = 3, label = None, counts = [])\n",
            "Expected type `str` but got `int`",
        ),
        (
            "    m = regex_match(1, line)\n    return None\n",
            "Expected type `str` but got `int`",
        ),
    ];
    for (body, expected) in cases {
        let d = install_error(Capability::Parser, &parser_source(body));
        assert_eq!(d.kind, DiagnosticKind::Type, "{d}");
        assert!(d.message.contains(expected), "{expected:?} not in {d}");
    }
    let uncalled = format!(
        "{PARSER_HEAD}def _never() -> int:\n    return \"x\"\ndef on_line(ctx: LineContext, line: str) -> Progress | None:\n    return None\n"
    );
    let d = install_error(Capability::Parser, &uncalled);
    assert_eq!(d.kind, DiagnosticKind::Type);
    assert!(d.message.contains("Expected type `int` but got `str`"), "{d}");
}

#[test]
fn entry_point_must_match_the_typed_host_contract() {
    let unannotated = format!("{PARSER_HEAD}def on_line(ctx, line):\n    return None\n");
    let d = install_error(Capability::Parser, &unannotated);
    assert!(
        d.message
            .contains("found `def(ctx: typing.Any, line: typing.Any) -> typing.Any`"),
        "{d}"
    );
    let wrong = format!("{PARSER_HEAD}def on_line(ctx: LineContext, line: int) -> Progress | None:\n    return None\n");
    assert_eq!(install_error(Capability::Parser, &wrong).kind, DiagnosticKind::Type);
    let missing = format!("{PARSER_HEAD}def parse(ctx: LineContext, line: str) -> Progress | None:\n    return None\n");
    assert!(
        install_error(Capability::Parser, &missing)
            .message
            .contains("missing entry point `on_line`")
    );
}

#[test]
fn values_from_untyped_json_are_checked_at_runtime() {
    // `event.data` is dict[str, typing.Any]: statically unknown, so the record
    // field annotation `prompt: str` is enforced when the action is built.
    let source = format!(
        "{WORKFLOW_HEAD}def on_event(ctx: WorkflowContext, event: Event) -> list[Action]:\n    return [start_session(\"a\", \"p\", event.data[\"n\"])]\n"
    );
    let engine = engine();
    let script = install(&engine, "runtime.star", Capability::Workflow, &source).unwrap();
    let d = engine
        .call(
            &script,
            &json!({"workflow": "wf", "owner": "coordinator", "state": {}}),
            &json!({"id": "e", "kind": "start", "data": {"n": 42}}),
        )
        .unwrap_err();
    assert_eq!(d.kind, DiagnosticKind::Type, "{d}");
    assert!(d.message.contains("does not match the type annotation"), "{d}");
    // Not every mistake is static: `str + int` in a body is a runtime error.
    let source = parser_source("    x = line + 1\n    return None\n");
    let script = install(&engine, "dynamic.star", Capability::Parser, &source).unwrap();
    let d = engine
        .call(
            &script,
            &json!({"command": "bazel build", "stream": "t", "provisional": false, "state": {}}),
            &json!("x"),
        )
        .unwrap_err();
    assert_eq!(d.kind, DiagnosticKind::Runtime);
}

#[test]
fn capabilities_are_separate_and_installation_is_explicit() {
    // A parser cannot load workflow action constructors.
    let parser = "load(\"@agentworks//workflow.star\", \"start_session\")\n".to_owned()
        + &parser_source("    return None\n")[PARSER_HEAD.len()..];
    assert_eq!(
        install_error(Capability::Parser, &parser).kind,
        DiagnosticKind::Capability
    );
    // A workflow has no pattern helper, and neither has print/json/I/O.
    for call in [
        "regex_match(\"a\", \"a\")",
        "print(\"x\")",
        "json.encode({})",
        "open(\"/etc/passwd\")",
    ] {
        let source = format!(
            "{WORKFLOW_HEAD}def on_event(ctx: WorkflowContext, event: Event) -> list[Action]:\n    {call}\n    return []\n"
        );
        let d = install_error(Capability::Workflow, &source);
        assert_eq!(d.kind, DiagnosticKind::Type, "{call}: {d}");
        assert!(d.message.contains("not found"), "{call}: {d}");
    }
    let engine = Engine::new().unwrap();
    let mut disabled = Installation::of("bazel_progress.star", Capability::Parser, BAZEL);
    disabled.enabled = false;
    assert_eq!(
        engine.compile(&disabled, BAZEL).err().unwrap().kind,
        DiagnosticKind::Install
    );
    let pinned = Installation::of("bazel_progress.star", Capability::Parser, BAZEL);
    let edited = BAZEL.replace("Executing", "Running");
    let d = engine.compile(&pinned, &edited).err().unwrap();
    assert!(d.message.contains("does not match the installed content hash"), "{d}");
}
