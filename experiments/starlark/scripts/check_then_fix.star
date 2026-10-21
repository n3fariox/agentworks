# Sample E-01 workflow: run a check session; only if it fails, run a fix session.
# Evaluation is a pure decision. Rust waits for sessions, persists every
# decision, authorizes actions at dispatch and owns cancellation and retries.
load("@agentworks//workflow.star", "Decision", "Event", "decide", "start_session")

def _finish(state: dict[str, typing.Any], outcome: str) -> Decision:
    return decide(dict(state, step = "done"), outcome = outcome)

def on_event(state: dict[str, typing.Any], event: Event) -> Decision:
    if event.kind == "start":
        project = event.data["project"]
        check = start_session("check", project, "Run the project checks and report any failures.")
        return decide({"project": project, "step": "check"}, actions = [check])
    if event.kind == "action_denied":
        return _finish(state, "denied")
    if event.kind != "action_completed":
        return decide(state)
    if event.outcome in ("cancelled", "interrupted"):
        # Interrupted work is reported, never blindly retried.
        return _finish(state, "stopped")
    if event.action_key == "check":
        if event.outcome == "succeeded":
            return _finish(state, "passed")
        summary = str(event.data.get("summary", "no summary"))[:500]
        fix = start_session("fix", state["project"], "Fix these check failures: " + summary)
        return decide(dict(state, step = "fix"), actions = [fix])
    if event.action_key == "fix":
        return _finish(state, "fixed" if event.outcome == "succeeded" else "fix_failed")
    return decide(state)
