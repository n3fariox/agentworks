//! Bazel parser script through the Rust-owned stream host. Cases mirror
//! tests/bazel-progress.test.mjs (U-05); each runs against both the Starlark
//! script and the built-in Rust baseline, which must agree.
mod common;
use std::sync::Arc;

use agentworks_starlark::engine::{Capability, DiagnosticKind};
use agentworks_starlark::progress::{BuiltinBazel, LineParser, StarlarkParser, ToolProgress};
use common::*;
use serde_json::{Value, json};

fn starlark() -> StarlarkParser {
    let engine = engine();
    let script = install(&engine, "bazel_progress.star", Capability::Parser, BAZEL).unwrap();
    StarlarkParser { engine, script }
}

fn parsers() -> Vec<(&'static str, Box<dyn LineParser>)> {
    vec![("starlark", Box::new(starlark())), ("builtin", Box::new(BuiltinBazel))]
}

fn build() -> ToolProgress {
    let mut t = ToolProgress::default();
    t.apply(&BuiltinBazel, &json!({"title": "bazel test //fixture:all", "rawInput": {"command": "bazel test //fixture:all"}, "status": "in_progress"}), None);
    t
}
fn delta(data: &str) -> Value {
    json!({"_meta": {"terminal_output_delta": {"terminal_id": "term-build", "data": data}}})
}
fn snapshot(data: &str) -> Value {
    json!({"_meta": {"terminal_output": {"terminal_id": "term-build", "data": data}}})
}

fn fixture() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../tests/fixtures/bazel-events.json")).unwrap()
}

#[test]
fn u05_fixture_output_before_start_ansi_cr_totals_snapshots_and_failure() {
    let mut views = Vec::new();
    for (name, parser) in parsers() {
        let mut tool = ToolProgress::default();
        for event in fixture().iter().filter(|e| e["kind"] == "acp") {
            tool.apply(parser.as_ref(), &event["data"], event["seq"].as_i64());
            match event["seq"].as_i64().unwrap() {
                4 => {
                    assert_eq!(
                        tool.view(),
                        "Bazel · Testing · 2 / 6 actions reported · 1 / 3 tests reported",
                        "{name}"
                    );
                    let raw = tool.raw("terminal:term-build").unwrap();
                    assert!(raw.contains("\x1b[32mLoading:") && raw.contains("\r\x1b[KAnalyzing:"));
                }
                5 => {
                    assert!(
                        tool.view().contains("3 / 8 actions reported · 2 / 5 tests reported"),
                        "{name}"
                    );
                    assert!(
                        !tool
                            .raw("terminal:term-build")
                            .unwrap()
                            .contains("Compiling fixture.cc")
                    );
                }
                _ => {}
            }
        }
        assert_eq!(tool.status.as_deref(), Some("failed"));
        assert_eq!(
            tool.view(),
            "Bazel · Testing · 4 / 9 actions reported · 3 / 5 tests reported · Exit 1",
            "{name}"
        );
        assert!(
            tool.raw("terminal:term-build")
                .unwrap()
                .contains("Build completed successfully")
        );
        assert!(!tool.view().contains('%'));
        views.push(tool.snapshot());
    }
    assert_eq!(views[0], views[1], "starlark and built-in disagree");
}

#[test]
fn output_before_the_command_is_known_is_parsed_once_recognised() {
    for (name, parser) in parsers() {
        let mut tool = ToolProgress::default();
        tool.apply(parser.as_ref(), &delta("Load"), Some(1));
        assert_eq!(tool.calls, 0);
        tool.apply(parser.as_ref(), &json!({"title": "bazel build //...", "rawInput": {"command": "bazel build //..."}, "status": "in_progress"}), Some(2));
        tool.apply(parser.as_ref(), &delta("ing: 3 packages loaded\r"), Some(3));
        assert_eq!(tool.view(), "Bazel · Loading", "{name}");
        assert_eq!(tool.raw("terminal:term-build").unwrap(), "Loading: 3 packages loaded\r");
    }
}

#[test]
fn every_chunk_boundary_gives_the_same_progress_and_exact_raw_bytes() {
    let text = "\x1b]0;[999 / 999] fake\x1b\\\x1b[32mLoading: 1 packages loaded\x1b[0m\r\x1b[K[1,234 / 2,000] 2 / 8 tests; test\n";
    let parser = starlark();
    let chars: Vec<char> = text.chars().collect();
    for at in 0..=chars.len() {
        let (a, b): (String, String) = (chars[..at].iter().collect(), chars[at..].iter().collect());
        let mut tool = build();
        tool.apply(&parser, &delta(&a), Some(1));
        tool.apply(&parser, &delta(&b), Some(2));
        assert_eq!(
            tool.view(),
            "Bazel · Testing · 1,234 / 2,000 actions reported · 2 / 8 tests reported",
            "split at {at}"
        );
        assert_eq!(tool.raw("terminal:term-build").unwrap(), text);
    }
}

#[test]
fn partial_counts_are_not_completed_and_totals_may_change() {
    for (name, parser) in parsers() {
        let mut tool = build();
        tool.apply(parser.as_ref(), &delta("[1 / 2"), Some(1));
        assert_eq!(tool.progress(), None, "{name}");
        tool.apply(parser.as_ref(), &delta("0] Compiling\r[2 / 30] Compiling\r"), Some(2));
        assert!(tool.view().contains("2 / 30 actions reported"), "{name}");
        tool.apply(parser.as_ref(), &delta("[1 / 3] restarted\r"), Some(3));
        assert!(tool.view().contains("1 / 3 actions reported"), "{name}");
        // Labels describe the current line only; the unterminated line is provisional.
        assert_eq!(tool.progress().unwrap().label, None);
        tool.apply(parser.as_ref(), &delta("[2 / 3] Linking //app"), Some(4));
        assert_eq!(
            tool.progress().unwrap().label.as_deref(),
            Some("Linking //app"),
            "{name}"
        );
    }
}

#[test]
fn snapshots_replace_and_duplicates_are_dropped_by_sequence() {
    let parser = starlark();
    let mut tool = build();
    tool.apply(&parser, &delta("[1 / 3] compile\r"), Some(1));
    tool.apply(&parser, &snapshot("Analyzing: fixture\r"), Some(2));
    assert_eq!(tool.view(), "Bazel · Analyzing");
    tool.apply(&parser, &snapshot(""), Some(3));
    assert_eq!(tool.view(), "In progress");
    let update = delta("[1 / 3] compile\n");
    assert!(tool.apply(&parser, &update, Some(4)));
    assert!(!tool.apply(&parser, &update, Some(4)), "duplicate sequence");
    assert!(
        !tool.apply(&parser, &json!({"status": "failed"}), Some(2)),
        "stale sequence"
    );
    assert!(
        tool.apply(&parser, &update, Some(5)),
        "identical new output is real output"
    );
    assert_eq!(tool.raw("terminal:term-build").unwrap(), "[1 / 3] compile\n".repeat(2));
    assert_eq!(tool.status.as_deref(), Some("in_progress"));
}

#[test]
fn invalid_counts_and_non_bazel_commands_produce_no_estimates() {
    for (name, parser) in parsers() {
        for text in [
            "[4 / 2] invalid",
            "[9007199254740992 / 9007199254740993] too large",
            "INFO: Build completed successfully",
        ] {
            let mut tool = build();
            tool.apply(parser.as_ref(), &delta(text), Some(1));
            assert_eq!(tool.progress(), None, "{name} {text}");
            assert_eq!(tool.status.as_deref(), Some("in_progress"));
        }
        let mut tool = build();
        tool.apply(parser.as_ref(), &delta("[0 / 0] prep"), Some(1));
        assert_eq!(tool.view(), "Bazel · Executing · 0 / 0 actions reported");
        let mut make = ToolProgress::default();
        make.apply(
            parser.as_ref(),
            &json!({"rawInput": {"command": "make all"}, "status": "in_progress"}),
            Some(1),
        );
        make.apply(parser.as_ref(), &snapshot("[1 / 2] arbitrary"), Some(2));
        assert_eq!(make.view(), "In progress", "{name}");
        assert_eq!(make.calls, 0, "non-Bazel output is never sent to a parser");
    }
}

#[test]
fn oversized_lines_are_bounded_and_parsing_recovers() {
    let parser = starlark();
    let mut tool = build();
    let text = "x".repeat(20000) + "[9 / 9]\n[2 / 4] compile";
    tool.apply(&parser, &delta(&text), Some(1));
    assert!(tool.view().contains("2 / 4 actions reported"));
    assert_eq!(tool.raw("terminal:term-build").unwrap(), text);
}

#[test]
fn replay_rebuilds_identical_progress() {
    let parser = starlark();
    let replay = || {
        let mut t = ToolProgress::default();
        for e in fixture().iter().filter(|e| e["kind"] == "acp") {
            t.apply(&parser, &e["data"], e["seq"].as_i64());
        }
        (t.snapshot(), t.raw("terminal:term-build").map(str::to_owned))
    };
    assert_eq!(replay(), replay());
}

/// Parser failures disable derived progress for that tool only. Raw output,
/// later events, tool status and exit metadata remain intact.
#[test]
fn parser_failure_is_isolated_and_attributable() {
    let engine = engine();
    let failing = [
        (
            "raises.star",
            "    if \"boom\" in line:\n        fail(\"cannot parse\")\n    return ParseResult(state = state, progress = Progress(phase = \"Loading\", label = None, counts = []))\n",
            DiagnosticKind::Runtime,
        ),
        (
            "loops.star",
            "    if \"boom\" in line:\n        for i in range(1000000000):\n            pass\n    return ParseResult(state = state, progress = Progress(phase = \"Loading\", label = None, counts = []))\n",
            DiagnosticKind::Limit,
        ),
        (
            "invalid.star",
            "    if \"boom\" in line:\n        return ParseResult(state = state, progress = Progress(phase = \"Loading\", label = None, counts = [Counts(done = 5, total = 2, unit = \"actions\")]))\n    return ParseResult(state = state, progress = Progress(phase = \"Loading\", label = None, counts = []))\n",
            DiagnosticKind::InvalidResult,
        ),
        (
            "huge.star",
            "    if \"boom\" in line:\n        return ParseResult(state = state, progress = Progress(phase = \"Loading\", label = line * 10000, counts = []))\n    return ParseResult(state = state, progress = Progress(phase = \"Loading\", label = None, counts = []))\n",
            DiagnosticKind::Limit,
        ),
    ];
    for (name, body, kind) in failing {
        let script = install(&engine, name, Capability::Parser, &parser_source(body)).unwrap();
        let parser = StarlarkParser {
            engine: Arc::clone(&engine),
            script,
        };
        let mut tool = build();
        tool.apply(&parser, &delta("ok\n"), Some(1));
        assert_eq!(tool.view(), "Bazel · Loading", "{name}");
        tool.apply(&parser, &delta("boom\n"), Some(2));
        let failure = tool.parser_failure.clone().expect(name);
        assert_eq!(failure.kind, kind, "{name}: {failure}");
        assert_eq!(failure.script, name);
        assert_eq!(tool.progress(), None);
        assert_eq!(tool.view(), "In progress", "{name}: elapsed/activity fallback");
        let calls = tool.calls;
        tool.apply(&parser, &json!({"status": "failed", "_meta": {"terminal_output_delta": {"terminal_id": "term-build", "data": "after\n"},
            "terminal_exit": {"terminal_id": "term-build", "exit_code": 2, "signal": null}}}), Some(3));
        assert_eq!(tool.calls, calls, "{name}: no further calls after failure");
        assert_eq!(tool.raw("terminal:term-build").unwrap(), "ok\nboom\nafter\n");
        assert_eq!(tool.status.as_deref(), Some("failed"));
        assert_eq!(tool.view(), "Exit 2");
    }
}
