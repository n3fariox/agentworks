load("@agentworks//workflow.star", "WorkflowContext", "Event", "Action", "parameter", "sessions_for_pr", "queue_input", "start_session", "pr_worktree", "review_prompt", "skip")

def review_requested(ctx: WorkflowContext, event: Event) -> list[Action]:
    pr = event.data["pr"]
    matches = sessions_for_pr(ctx.sessions, pr, archived = False)
    if len(matches) > 1:
        return [skip("Multiple matching sessions")]
    prompt = review_prompt(pr, ctx.instructions)
    if matches:
        return [queue_input(matches[0], prompt)]
    return [start_session(settings = ctx.settings, workspace = pr_worktree(pr), prompt = prompt)]

agent.register_github_trigger(
    review_requested,
    host = parameter("host"),
    repo = parameter("repo"),
    account = parameter("account"),
    events = ["pr.assigned_to_me", "pr.review_requested"],
)
