use agentworks_starlark::engine::Engine;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
const SOURCE: &str = r#"
load("@agentworks//tool.star", "ToolContext", "ToolCall")
def tool_header(ctx: ToolContext, tool: ToolCall) -> str | None:
    if tool.title == "set":
        agent.set_badge("review", "Needs review", tone = "warning", url = "https://github.com/org/repo/pull/1")
    elif tool.title == "clear":
        agent.clear_badge("review")
    elif tool.title == "fail":
        agent.set_badge("review", "Must not commit")
        return str(1 // 0)
    return None
agent.register_tool_header(tool_header)
"#;
#[test]
fn existing_hooks_stage_set_and_clear_and_failures_discard_effects() {
    let engine = Engine::new().unwrap();
    let installed = engine.compile_registered("review.star", SOURCE).unwrap();
    let script = &installed.handlers[0];
    let ctx = json!({"provider":"demo","cwd":"/work","state":{}});
    let input = |title: &str| json!({"title":title,"kind":"other","command":null,"description":null,"input":{}});
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = seen.clone();
    engine.set_badge_sink(Arc::new(move |session, source, _, effects| {
        capture
            .lock()
            .unwrap()
            .push(json!({"session":session,"source":source,"effects":effects}))
    }));
    let _scope = engine.badge_scope(Some("owned-session"));
    let set = engine.call(script, &ctx, &input("set")).unwrap().0;
    assert!(
        seen.lock().unwrap().is_empty(),
        "effects must not auto-commit before host validation"
    );
    engine.publish_badges(script, &set);
    assert_eq!(seen.lock().unwrap()[0]["session"], "owned-session");
    assert_eq!(seen.lock().unwrap()[0]["source"], "review.star");
    assert_eq!(set["badgeEffects"][0]["op"], "set");
    assert_eq!(set["badgeEffects"][0]["url"], "https://github.com/org/repo/pull/1");
    let clear = engine.call(script, &ctx, &input("clear")).unwrap().0;
    engine.publish_badges(script, &clear);
    assert_eq!(seen.lock().unwrap()[1]["effects"][0]["op"], "clear");
    assert!(engine.call(script, &ctx, &input("fail")).is_err());
    assert_eq!(seen.lock().unwrap().len(), 2);
}
#[test]
fn provisional_parser_lines_and_installation_cannot_commit_badges() {
    let engine = Engine::new().unwrap();
    let source = r#"load("@agentworks//progress.star", "LineContext", "Progress")
def on_line(ctx: LineContext, line: str) -> Progress | None:
    agent.set_badge("build", line)
    return None
agent.register_progress(on_line, command_pattern = ".*")
"#;
    let scripts = engine.compile_registered("build.star", source).unwrap();
    let ctx = json!({"command":"make","stream":"stdout","state":{},"provisional":true});
    assert!(
        engine
            .call(&scripts.handlers[0], &ctx, &json!("failed"))
            .unwrap()
            .0
            .get("badgeEffects")
            .is_none()
    );
    let source = source.to_owned() + "\nagent.set_badge(\"bad\", \"outside callback\")\n";
    assert!(engine.compile_registered("bad.star", &source).is_err());
}
