# Sample E-01 workflow: run a check session; only if it fails, run a fix session.
# Evaluation is a pure decision. Rust waits for sessions, persists every
# decision, authorizes actions at dispatch and owns cancellation and retries.
load("@agentworks//workflow.star", "Action", "Event", "WorkflowContext", "finish", "start_session")

def _done(ctx: WorkflowContext, outcome: str) -> list[Action]:
    ctx.state["step"] = "done"
    return [finish(outcome)]

def on_event(ctx: WorkflowContext, event: Event) -> list[Action]:
    state = ctx.state
    if event.kind == "start":
        state["project"] = event.data["project"]
        state["step"] = "check"
        return [start_session("check", state["project"], "Run the project checks and report any failures.")]
    if event.kind == "action_denied":
        return _done(ctx, "denied")
    if event.kind != "action_completed":
        return []
    if event.outcome in ("cancelled", "interrupted"):
        # Interrupted work is reported, never blindly retried.
        return _done(ctx, "stopped")
    if event.action_key == "check":
        if event.outcome == "succeeded":
            return _done(ctx, "passed")
        summary = str(event.data.get("summary", "no summary"))[:500]
        state["step"] = "fix"
        return [start_session("fix", state["project"], "Fix these check failures: " + summary)]
    if event.action_key == "fix":
        return _done(ctx, "fixed" if event.outcome == "succeeded" else "fix_failed")
    return []
