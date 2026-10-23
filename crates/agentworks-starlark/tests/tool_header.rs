use agentworks_starlark::TOOL_HEADER_SCRIPT;
use agentworks_starlark::engine::{Capability, Engine};
use serde_json::{Value, json};

fn header(engine: &Engine, provider: &str, tool: Value) -> Value {
    let installed = engine
        .compile_registered("tool_header.star", TOOL_HEADER_SCRIPT)
        .unwrap();
    let script = &installed.handlers[0];
    assert_eq!(script.capability, Capability::ToolHeader);
    assert!(script.text_pattern.as_ref().unwrap().is_match("anything"));
    let mut tool = tool;
    tool["kind"] = tool.get("kind").cloned().unwrap_or(json!("execute"));
    tool["input"] = tool.get("input").cloned().unwrap_or(json!({}));
    let (value, _) = engine
        .call(
            script,
            &json!({"provider":provider,"cwd":"/home/me/project","state":{}}),
            &tool,
        )
        .unwrap();
    value["result"].clone()
}

#[test]
fn claude_uses_the_command_description() {
    let engine = Engine::new().unwrap();
    let command = "wc -l web/side-panel.js; grep -n \"open(\\|hidden\" web/side-panel.js | head -40 | cut -c1-200";
    assert_eq!(
        header(
            &engine,
            "claude",
            json!({"title":command,"command":command,"description":"Read side panel open logic"})
        ),
        "Read side panel open logic"
    );
}

#[test]
fn codex_commands_are_reduced_to_their_first_meaningful_step() {
    let engine = Engine::new().unwrap();
    for (command, expected) in [
        (
            "ZIG_GLOBAL_CACHE_DIR=/tmp/zig ZIG_LOCAL_CACHE_DIR=/tmp/zig-local mise exec -- cargo build --locked --offline",
            json!("cargo build --locked --offline"),
        ),
        (
            "\"npx playwright test tests/sessions.spec.mjs --grep 'mobile: drawer' \"",
            json!("npx playwright test tests/sessions.spec.mjs --grep 'mobile: drawer'"),
        ),
        (
            "ls -ld .cache 2>/dev/null; ls -l target/debug/agentworks; rg -n \"A|B\" crates | head",
            json!("ls -ld .cache 2>/dev/null +3"),
        ),
        (
            "cd /home/me/project/web && npm test 2>&1 | tail -5",
            json!("npm test 2>&1 +1"),
        ),
        (
            "cat package.json && sed -n '1,180p' playwright.config.*",
            json!("cat package.json +1"),
        ),
        ("bash -lc 'cargo test -p agentworks'", json!("cargo test -p agentworks")),
        (
            "PID=$(ss -ltnp | grep ':4395 ' | cut -d= -f2); kill -INT $PID; git status --short | wc -l",
            json!("kill -INT $PID +2"),
        ),
        ("A=$(date) B=\"x y\" make -j8", json!("make -j8")),
        // Unbalanced quotes stay one step; an unchanged title needs no header.
        ("echo \"unbalanced; ls", Value::Null),
        ("FOO=1 echo \"unbalanced", json!("echo \"unbalanced")),
    ] {
        assert_eq!(
            header(&engine, "codex", json!({"title":command,"command":command})),
            expected,
            "{command}"
        );
    }
    let long = format!("cargo test {}", "x".repeat(200));
    let shortened = header(&engine, "codex", json!({"title":long,"command":long}));
    assert!(shortened.as_str().unwrap().ends_with('…'));
    assert!(shortened.as_str().unwrap().chars().count() <= 100);
}

#[test]
fn titles_lose_the_session_directory_and_unchanged_titles_return_none() {
    let engine = Engine::new().unwrap();
    assert_eq!(
        header(
            &engine,
            "codex",
            json!({"kind":"read","title":"Read file '/home/me/project/scripts/verify.mjs'","input":{"path":"/home/me/project/scripts/verify.mjs"}})
        ),
        "Read file 'scripts/verify.mjs'"
    );
    assert_eq!(
        header(
            &engine,
            "codex",
            json!({"title":"mcp.agentworks.restart_service","input":{"server":"agentworks"}})
        ),
        Value::Null
    );
    assert_eq!(
        header(&engine, "claude", json!({"title":"ls","command":"ls"})),
        Value::Null
    );
}

#[test]
fn header_hooks_are_typed_and_routed_by_title() {
    let engine = Engine::new().unwrap();
    let source = "load(\"@agentworks//tool.star\", \"ToolCall\", \"ToolContext\")\n\
        def bazel(ctx: ToolContext, tool: ToolCall) -> str | None:\n    return \"Bazel\"\n\
        agent.register_tool_header(bazel, title_pattern = \"\\\\bbazel\\\\b\")\n";
    let installed = engine.compile_registered("bazel_header.star", source).unwrap();
    assert_eq!(installed.hooks[0].hook, "tool_header");
    let pattern = installed.handlers[0].text_pattern.as_ref().unwrap();
    assert!(pattern.is_match("bazel test //..."));
    assert!(!pattern.is_match("cargo test"));
    let wrong = source.replace("-> str | None", "-> str");
    assert!(engine.compile_registered("wrong.star", &wrong).is_err());
}
