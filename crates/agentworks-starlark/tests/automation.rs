use agentworks_starlark::{GITHUB_REVIEW_SCRIPT, engine::Engine};
use serde_json::json;

#[test]
fn script_registers_subscription_and_composes_data_only_primitives() {
    let engine = Engine::new().unwrap();
    let recipe = engine
        .compile_automation("custom_review.star", GITHUB_REVIEW_SCRIPT)
        .unwrap();
    assert_eq!(recipe.hooks[0].hook, "github_trigger");
    assert_eq!(recipe.hooks[0].trigger.as_ref().unwrap()["host"], "$host");
    let pr = json!({"host":"github.com","owner":"team","repo":"repo","number":17,"headSha":"a".repeat(40),"url":"https://github.com/team/repo/pull/17"});
    let event = json!({"id":"event","kind":"pr.review_requested","data":{"pr":pr}});
    let mut ctx = json!({"workflow":"run","owner":"human","state":{},"settings":{"provider":"demo","projectId":"p"},"sessions":[],"instructions":"Check correctness"});
    let (value, _) = engine.call(&recipe.handlers[0], &ctx, &event).unwrap();
    assert_eq!(value["result"][0]["kind"], "start_session");
    assert_eq!(value["result"][0]["workspace"]["pullRequest"]["headSha"], pr["headSha"]);
    let session = json!({"id":"one","archived":false,"pullRequests":[{"host":"GITHUB.COM","owner":"TEAM","repo":"repo","number":17}]});
    ctx["sessions"] = json!([session]);
    let (value, _) = engine.call(&recipe.handlers[0], &ctx, &event).unwrap();
    assert_eq!(value["result"][0]["kind"], "queue_input");
    assert_eq!(value["result"][0]["target"], "one");
    ctx["sessions"] = json!([session, session]);
    let (value, _) = engine.call(&recipe.handlers[0], &ctx, &event).unwrap();
    assert_eq!(value["result"][0]["kind"], "skip");
    ctx["sessions"][0]["archived"] = json!(true);
    let (value, _) = engine.call(&recipe.handlers[0], &ctx, &event).unwrap();
    assert_eq!(value["result"][0]["kind"], "queue_input");
    for replacement in ["events = []", "events = [\"push\"]"] {
        let source =
            GITHUB_REVIEW_SCRIPT.replace("events = [\"pr.assigned_to_me\", \"pr.review_requested\"]", replacement);
        assert!(engine.compile_automation("invalid.star", &source).is_err());
    }
}

#[test]
fn script_kinds_are_enforced_and_mixed_registrations_are_rejected() {
    let engine = Engine::new().unwrap();
    assert!(
        engine
            .compile_registered("review.star", GITHUB_REVIEW_SCRIPT)
            .err()
            .unwrap()
            .message
            .contains("Hook scripts cannot")
    );
    let hook = "load(\"@agentworks//github.star\", \"GithubContext\")\ndef hook(ctx: GithubContext, event: dict[str, typing.Any]) -> None:\n    return None\nagent.register_github(hook)\n";
    assert!(
        engine
            .compile_automation("hook.star", hook)
            .err()
            .unwrap()
            .message
            .contains("only register automation")
    );
    let mixed = format!("{GITHUB_REVIEW_SCRIPT}\n{hook}");
    assert!(engine.compile_registered("mixed.star", &mixed).is_err());
    assert!(engine.compile_automation("mixed.star", &mixed).is_err());
    let duplicate = format!(
        "{GITHUB_REVIEW_SCRIPT}\nagent.register_github_trigger(review_requested,host=\"github.com\",repo=\"team/repo\",account=\"fixture\",events=[\"pr.review_requested\"])\n"
    );
    assert!(engine.compile_automation("duplicate.star", &duplicate).is_err());
}
