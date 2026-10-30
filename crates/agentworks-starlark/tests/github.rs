use agentworks_starlark::engine::Engine;
use serde_json::json;
#[test]
fn github_observers_are_typed_and_can_stage_badge_setters_without_actions() {
    let engine = Engine::new().unwrap();
    let source = r#"load("@agentworks//github.star", "GithubContext")
def on_github(ctx: GithubContext, event: dict[str, typing.Any]) -> None:
    if event["kind"] == "pr.status_changed":
        agent.set_badge("review", "Review changed", tone = "warning")
    return None
agent.register_github(on_github)
"#;
    let installed = engine.compile_registered("pr.star", source).unwrap();
    assert_eq!(installed.hooks[0].hook, "github");
    let value = engine
        .call(
            &installed.handlers[0],
            &json!({"provider":"demo","cwd":"/tmp","state":{}}),
            &json!({"id":42,"kind":"pr.status_changed","initial":false}),
        )
        .unwrap()
        .0;
    assert!(value["result"].is_null());
    assert_eq!(value["badgeEffects"][0]["id"], "review");
    let broken = source
        .replace("-> None:", "-> str:")
        .replace("return None", "return \"wrong\"");
    assert!(engine.compile_registered("bad.star", &broken).is_err());
    assert!(
        engine
            .compile_registered(
                "io.star",
                &source.replace("return None", "return Action(kind = \"start_session\")")
            )
            .is_err()
    );
}

#[test]
fn github_event_filters_are_exact_optional_and_checked_at_installation() {
    let engine = Engine::new().unwrap();
    let source = r#"load("@agentworks//github.star", "GithubContext")
def changed(ctx: GithubContext, event: dict[str, typing.Any]) -> None:
    return None
agent.register_github(changed, events = ["pr.status_changed", "pr.unavailable"])
"#;
    let compiled = engine.compile_registered("filtered.star", source).unwrap();
    assert_eq!(
        compiled.hooks[0].events,
        Some(vec!["pr.status_changed".into(), "pr.unavailable".into()])
    );
    assert!(compiled.handlers[0].accepts_github_event("pr.status_changed"));
    assert!(!compiled.handlers[0].accepts_github_event("pr.snapshot"));
    for filter in ["[]", "[\"pr.typo\"]", "\"pr.status_changed\"", "[17]"] {
        assert!(
            engine
                .compile_registered(
                    "bad_filter.star",
                    &source.replace("[\"pr.status_changed\", \"pr.unavailable\"]", filter)
                )
                .is_err(),
            "must reject {filter}"
        );
    }
    for declaration in [
        "agent.register_github(changed)",
        "agent.register_github(changed, events = None)",
    ] {
        let all = engine
            .compile_registered(
                "all.star",
                &source.replace(
                    "agent.register_github(changed, events = [\"pr.status_changed\", \"pr.unavailable\"])",
                    declaration,
                ),
            )
            .unwrap();
        assert!(all.handlers[0].accepts_github_event("pr.snapshot"));
        assert!(all.hooks[0].events.is_none());
    }
}
